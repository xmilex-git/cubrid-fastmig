//! Scans the heaps of many classes at once and turns every live row into the (key, OID) runs of each of its
//! class's indexes, inside the memory limit.
//!
//! Input: per class its heap's user pages grouped by sector (workspace#490 `FileTable::user_pages_by_sector`,
//! header and bestspace pages left out by the caller) and the key order of each of its indexes; a row callback
//! that reads one page of one class and extracts each index's key (workspace#491). Output: per class, per index
//! its runs (`IndexRuns`) and the count of rows with an all-NULL key.
//! Work units: a class's sectors cut into ranges of `UNIT_SECTORS`, classes largest first (LPT), every unit of every
//! class in one queue that `--workers` threads drain — a large class spreads over the workers, hundreds of
//! one-sector classes run side by side (workspace#503). A worker keeps one writer per index of the class it is on
//! and finishes them when the next unit is another class's; the results are merged once at the end (PAR-17).
//! Memory (workspace#493, #503): each worker holds one share of the memory limit for the whole scan, split among the
//! writers of its current class; a writer spills past its part. A finished run stays in memory only if the limit
//! has room for it at that moment, otherwise it is spilled — the scan never waits for memory that only the build
//! after it frees, and a spill file exists only for keys that did not fit.
//! Source: R6 § 2 steps 1–3 (engine `get_next_vpid` sector distribution, `btree_sort_get_next`), § 4.1:
//! R-ROW-3 all-NULL keys are counted and left out, R-ROW-4 a NULL in a NOT NULL (PK) key stops the build.

use crate::build_error::BuildError;
use crate::intermediate_merge::MERGE_BUFFER;
use crate::key_order::KeyOrder;
use crate::memory_limit::MemoryLimit;
use crate::run_writer::{IndexRuns, RunWriter};
use crate::spill_dir::SpillDir;
use crate::worker_limit::WorkerLimit;
use fastmig_ondisk::{Oid, Vpid, Vsid};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Memory one worker asks for at most; it gets less when the memory limit is smaller.
pub const WORKER_SHARE: usize = 256 << 20;

/// The least share a scan worker runs with: the spill write buffer and a run as large.
pub const SCAN_MIN: usize = 2 * MERGE_BUFFER;

/// Sectors per work unit (64 pages each at 16 KiB): small enough to spread a large heap, large enough that the
/// queue costs nothing next to the pages.
const UNIT_SECTORS: usize = 4;

/// What the key extractor found for one index of one row.
pub enum RowKey<'a> {
    Key(&'a [u8]),
    /// Every key column NULL: not in the tree, counted in `num_nulls`.
    AllNull,
    /// A NULL column in a NOT NULL key.
    NotNullViolation,
}

/// One class to scan: its heap pages by sector and the key order of each of its indexes.
pub struct ScanClass<'o> {
    pub sectors: Vec<(Vsid, Vec<Vpid>)>,
    pub orders: Vec<&'o dyn KeyOrder>,
}

impl ScanClass<'_> {
    fn pages(&self) -> usize {
        self.sectors.iter().map(|(_, p)| p.len()).sum()
    }
}

/// Per index of one class: its runs and the all-NULL row count.
pub struct ClassRuns<'m> {
    pub runs: Vec<IndexRuns<'m>>,
    pub nulls: Vec<u64>,
}

/// A worker's writers for the class it is on.
struct Writers<'a> {
    class: usize,
    writers: Vec<RunWriter<'a>>,
    nulls: Vec<u64>,
}

/// `page_rows(state, class, vpid, emit)` reads one heap page of class `class` (index into `classes`) and calls
/// `emit(index, oid, key)` for every live row and index of that class; `state` is the worker's own (buffers it
/// reuses), made once per worker by `worker_state`. Returns the runs in `classes` order.
pub fn scan_classes<'m, S, F>(
    classes: &[ScanClass],
    workers: &WorkerLimit,
    memory: &'m MemoryLimit,
    spill: &SpillDir,
    worker_state: impl Fn() -> S + Sync,
    page_rows: F,
) -> Result<Vec<ClassRuns<'m>>, BuildError>
where
    F: Fn(&mut S, usize, Vpid, &mut dyn FnMut(usize, Oid, RowKey)) -> Result<(), BuildError> + Sync,
{
    let mut by_size: Vec<usize> = (0..classes.len()).collect();
    by_size.sort_by_key(|&c| std::cmp::Reverse(classes[c].pages()));
    let units: Vec<(usize, usize, usize)> = by_size
        .iter()
        .flat_map(|&c| {
            let n = classes[c].sectors.len();
            (0..n)
                .step_by(UNIT_SECTORS)
                .map(move |s| (c, s, (s + UNIT_SECTORS).min(n)))
        })
        .collect();
    let mut permits = workers.acquire(units.len().max(1));
    // half the limit for the workers' shares, the other half for finished runs kept in memory; the shares are taken
    // together and only as many workers run as get SCAN_MIN each: a worker with less spills one pair per run (the
    // verifier at --mem 16M after its live row map: heap peak 1.1 GiB, workspace#526)
    let share = WORKER_SHARE.min(memory.limit() / (2 * permits.count));
    let mut total = memory.try_acquire(share * permits.count);
    let fed = (total.bytes / SCAN_MIN).min(permits.count);
    if fed == 0 {
        return Err(BuildError::Io(std::io::Error::other(format!(
            "--mem leaves {} B for the heap scan, which needs at least {SCAN_MIN} B",
            total.bytes
        ))));
    }
    if fed < permits.count {
        drop(permits);
        permits = workers.acquire(fed);
    }
    let each = total.bytes / permits.count;
    let grants: Mutex<Vec<_>> = Mutex::new((0..permits.count).map(|_| total.split(each)).collect());
    drop(total);
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let first_err: Mutex<Option<BuildError>> = Mutex::new(None);
    let out: Mutex<Vec<ClassRuns<'m>>> = Mutex::new(
        classes
            .iter()
            .map(|c| ClassRuns {
                runs: (0..c.orders.len()).map(|_| IndexRuns::default()).collect(),
                nulls: vec![0; c.orders.len()],
            })
            .collect(),
    );
    std::thread::scope(|s| {
        for _ in 0..permits.count {
            let (units, next, failed, first_err, out, page_rows, worker_state) = (
                &units,
                &next,
                &failed,
                &first_err,
                &out,
                &page_rows,
                &worker_state,
            );
            let grants = &grants;
            s.spawn(move || {
                let grant = grants.lock().unwrap().pop().unwrap();
                let mut state = worker_state();
                let mut done: Vec<(usize, Vec<IndexRuns<'m>>, Vec<u64>)> = Vec::new();
                let mut cur: Option<Writers> = None;
                let r = (|| -> Result<(), BuildError> {
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        if i >= units.len() || failed.load(Ordering::Relaxed) {
                            break;
                        }
                        let (c, from, to) = units[i];
                        if cur.as_ref().is_none_or(|w| w.class != c) {
                            if let Some(w) = cur.take() {
                                done.push(finish(w, memory)?);
                            }
                            let orders = &classes[c].orders;
                            // the share also pays the write buffer of the one spill this worker makes at a time
                            let part =
                                grant.bytes.saturating_sub(MERGE_BUFFER) / orders.len().max(1);
                            cur = Some(Writers {
                                class: c,
                                writers: orders
                                    .iter()
                                    .map(|o| RunWriter::new(*o, spill, part))
                                    .collect(),
                                nulls: vec![0; orders.len()],
                            });
                        }
                        let w = cur.as_mut().unwrap();
                        let mut err: Option<BuildError> = None;
                        for (_, pages) in &classes[c].sectors[from..to] {
                            for &vpid in pages {
                                page_rows(&mut state, c, vpid, &mut |ix, oid, key| match key {
                                    RowKey::Key(k) => {
                                        if let Err(e) = w.writers[ix].push(k, oid) {
                                            err.get_or_insert(e.into());
                                        }
                                    }
                                    RowKey::AllNull => w.nulls[ix] += 1,
                                    RowKey::NotNullViolation => {
                                        err.get_or_insert(BuildError::NotNullViolation { oid });
                                    }
                                })?;
                                if let Some(e) = err.take() {
                                    return Err(e);
                                }
                            }
                        }
                    }
                    if let Some(w) = cur.take() {
                        done.push(finish(w, memory)?);
                    }
                    Ok(())
                })();
                drop(grant);
                match r {
                    Ok(()) => {
                        let mut o = out.lock().unwrap();
                        for (c, runs, nulls) in done {
                            for (ix, (run, n)) in runs.into_iter().zip(nulls).enumerate() {
                                o[c].runs[ix].append(run);
                                o[c].nulls[ix] += n;
                            }
                        }
                    }
                    Err(e) => {
                        failed.store(true, Ordering::Relaxed);
                        first_err.lock().unwrap().get_or_insert(e);
                    }
                }
            });
        }
    });
    match first_err.into_inner().unwrap() {
        Some(e) => Err(e),
        None => Ok(out.into_inner().unwrap()),
    }
}

/// A worker's writers of one class, finished: (class, runs per index, all-NULL rows per index).
fn finish<'m>(
    w: Writers,
    memory: &'m MemoryLimit,
) -> Result<(usize, Vec<IndexRuns<'m>>, Vec<u64>), BuildError> {
    let runs = w
        .writers
        .into_iter()
        .map(|wr| wr.finish_within(memory))
        .collect::<std::io::Result<_>>()?;
    Ok((w.class, runs, w.nulls))
}
