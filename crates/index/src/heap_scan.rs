//! Scans a class's heap pages sector by sector in parallel and turns every live row into the (key, OID) runs of
//! each of the class's indexes in one pass, inside the memory limit.
//!
//! Input: the heap's user pages grouped by sector (workspace#490 `FileTable::user_pages_by_sector`, header and
//! bestspace pages left out by the caller), the key order of each index, and a row callback that extracts each
//! index's key (workspace#491). Output: per index, its runs (`IndexRuns`) and the count of rows with an all-NULL key.
//! Source: R6 § 2 steps 1–3 (engine `get_next_vpid` sector distribution, `btree_sort_get_next`), § 4.1:
//! R-ROW-3 all-NULL keys are counted and left out, R-ROW-4 a NULL in a NOT NULL (PK) key stops the build.
//! Memory (workspace#493): the scan waits for one grant for all its writers — the keys of every index of the
//! class come out at once — and each (worker, index) writer spills past its equal share.

use crate::build_error::BuildError;
use crate::key_order::KeyOrder;
use crate::memory_limit::MemoryLimit;
use crate::run_writer::{IndexRuns, RunWriter};
use crate::spill_dir::SpillDir;
use crate::worker_limit::WorkerLimit;
use fastmig_ondisk::{Oid, Vpid, Vsid};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Memory one writer asks for; the grant is smaller when the memory limit is.
const WRITER_SHARE: usize = 64 << 20;

/// What the key extractor found for one index of one row.
pub enum RowKey<'a> {
    Key(&'a [u8]),
    /// Every key column NULL: not in the tree, counted in `num_nulls`.
    AllNull,
    /// A NULL column in a NOT NULL key.
    NotNullViolation,
}

/// Per index: its runs and the all-NULL row count.
pub struct ClassRuns<'m> {
    pub runs: Vec<IndexRuns<'m>>,
    pub nulls: Vec<u64>,
}

/// `page_rows(vpid, emit)` reads one heap page and calls `emit(index, oid, key)` for every live row and index.
pub fn scan_class<'m, F>(
    sectors: &[(Vsid, Vec<Vpid>)],
    orders: &[&dyn KeyOrder],
    workers: &WorkerLimit,
    memory: &'m MemoryLimit,
    spill: &SpillDir,
    page_rows: F,
) -> Result<ClassRuns<'m>, BuildError>
where
    F: Fn(Vpid, &mut dyn FnMut(usize, Oid, RowKey)) -> Result<(), BuildError> + Sync,
{
    let indexes = orders.len();
    let permits = workers.acquire(sectors.len());
    let writers = permits.count * indexes;
    let mut grant = memory.acquire(writers * WRITER_SHARE);
    let share = grant.bytes / writers.max(1);
    let grants: Vec<Mutex<_>> = (0..writers)
        .map(|_| Mutex::new(Some(grant.split(share))))
        .collect();
    drop(grant);
    let next = AtomicUsize::new(0);
    let out = Mutex::new(ClassRuns {
        runs: (0..indexes).map(|_| IndexRuns::default()).collect(),
        nulls: vec![0; indexes],
    });
    let failed: Mutex<Option<BuildError>> = Mutex::new(None);
    std::thread::scope(|s| {
        for w in 0..permits.count {
            let (grants, next, out, failed, page_rows) =
                (&grants, &next, &out, &failed, &page_rows);
            s.spawn(move || {
                let mut ws: Vec<RunWriter> = orders
                    .iter()
                    .map(|o| RunWriter::new(*o, spill, share))
                    .collect();
                let mut nulls = vec![0u64; indexes];
                let mut err: Option<BuildError> = None;
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= sectors.len() || failed.lock().unwrap().is_some() {
                        break;
                    }
                    for &vpid in &sectors[i].1 {
                        let r = page_rows(vpid, &mut |ix, oid, key| match key {
                            RowKey::Key(k) => {
                                if let Err(e) = ws[ix].push(k, oid) {
                                    err.get_or_insert(e.into());
                                }
                            }
                            RowKey::AllNull => nulls[ix] += 1,
                            RowKey::NotNullViolation => {
                                err.get_or_insert(BuildError::NotNullViolation { oid });
                            }
                        });
                        if let Some(e) = r.err().or(err.take()) {
                            failed.lock().unwrap().get_or_insert(e);
                            return;
                        }
                    }
                }
                let mut o = out.lock().unwrap();
                for (ix, wr) in ws.into_iter().enumerate() {
                    let g = grants[w * indexes + ix].lock().unwrap().take().unwrap();
                    o.runs[ix].append(wr.finish(g));
                    o.nulls[ix] += nulls[ix];
                }
            });
        }
    });
    match failed.into_inner().unwrap() {
        Some(e) => Err(e),
        None => Ok(out.into_inner().unwrap()),
    }
}
