//! Scans a class's heap pages sector by sector in parallel and turns every live row into the (key, OID) runs of
//! each of the class's indexes in one pass.
//!
//! Input: the heap's user pages grouped by sector (workspace#490 `FileTable::user_pages_by_sector`, header and
//! bestspace pages left out by the caller) and a row callback that extracts each index's key (workspace#491).
//! Output: per index, one run per worker and the count of rows with an all-NULL key.
//! Source: R6 § 2 steps 1–3 (engine `get_next_vpid` sector distribution, `btree_sort_get_next`), § 4.1:
//! R-ROW-3 all-NULL keys are counted and left out, R-ROW-4 a NULL in a NOT NULL (PK) key stops the build.

use crate::build_error::BuildError;
use crate::sort_run::Run;
use crate::worker_budget::WorkerBudget;
use fastmig_ondisk::{Oid, Vpid, Vsid};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// What the key extractor found for one index of one row.
pub enum RowKey<'a> {
    Key(&'a [u8]),
    /// Every key column NULL: not in the tree, counted in `num_nulls`.
    AllNull,
    /// A NULL column in a NOT NULL key.
    NotNullViolation,
}

/// Per index: the runs of every worker and the all-NULL row count.
pub struct ClassRuns {
    pub runs: Vec<Vec<Run>>,
    pub nulls: Vec<u64>,
}

/// `page_rows(vpid, emit)` reads one heap page and calls `emit(index, oid, key)` for every live row and index.
pub fn scan_class<F>(
    sectors: &[(Vsid, Vec<Vpid>)],
    indexes: usize,
    budget: &WorkerBudget,
    page_rows: F,
) -> Result<ClassRuns, BuildError>
where
    F: Fn(Vpid, &mut dyn FnMut(usize, Oid, RowKey)) -> Result<(), BuildError> + Sync,
{
    let permits = budget.acquire(sectors.len());
    let next = AtomicUsize::new(0);
    let out = Mutex::new(ClassRuns {
        runs: (0..indexes).map(|_| Vec::new()).collect(),
        nulls: vec![0; indexes],
    });
    let failed: Mutex<Option<BuildError>> = Mutex::new(None);
    std::thread::scope(|s| {
        for _ in 0..permits.count {
            s.spawn(|| {
                let mut runs: Vec<Run> = (0..indexes).map(|_| Run::default()).collect();
                let mut nulls = vec![0u64; indexes];
                let mut violation: Option<BuildError> = None;
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= sectors.len() || failed.lock().unwrap().is_some() {
                        break;
                    }
                    for &vpid in &sectors[i].1 {
                        let r = page_rows(vpid, &mut |ix, oid, key| match key {
                            RowKey::Key(k) => runs[ix].push(k, oid),
                            RowKey::AllNull => nulls[ix] += 1,
                            RowKey::NotNullViolation => {
                                violation.get_or_insert(BuildError::NotNullViolation { oid });
                            }
                        });
                        if let Some(e) = r.err().or(violation.take()) {
                            failed.lock().unwrap().get_or_insert(e);
                            return;
                        }
                    }
                }
                let mut o = out.lock().unwrap();
                for (ix, run) in runs.into_iter().enumerate() {
                    o.runs[ix].push(run);
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
