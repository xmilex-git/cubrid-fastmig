//! Builds several indexes at the same time under the one worker limit.
//!
//! Input: one job per index. Output: each job's result, in job order.
//! Source: workspace#481 G1 (`--workers` caps every thread; indexes build concurrently, R6 § 2 step 0 notes the
//! engine builds one at a time), workspace#503 (a pool of `--workers` threads takes the jobs largest first, no
//! thread per index). Each build holds one permit for its serial steps and borrows free permits for its parallel
//! ones (`WorkerLimit::try_acquire`), so builds never wait on each other's parallel steps; once fewer builds than
//! workers remain, the large ones borrow the idle permits. Their keys are already in memory grants or spill files
//! (`IndexRuns`); a build only borrows merge buffers on top.

use crate::build::{BuiltIndex, IndexJob, build_index};
use crate::build_error::BuildError;
use crate::job_pool::run_jobs;
use crate::memory_limit::MemoryLimit;
use crate::worker_limit::WorkerLimit;

pub fn build_indexes(
    jobs: Vec<IndexJob>,
    workers: &WorkerLimit,
    memory: &MemoryLimit,
) -> Vec<Result<BuiltIndex, BuildError>> {
    // largest first (LPT); results go back to job order
    let mut order: Vec<(usize, IndexJob)> = jobs.into_iter().enumerate().collect();
    order.sort_by_key(|(_, j)| std::cmp::Reverse(j.runs.entries()));
    let (at, jobs): (Vec<usize>, Vec<IndexJob>) = order.into_iter().unzip();
    let built = run_jobs(workers.limit(), jobs, |j| build_index(j, workers, memory));
    let mut out: Vec<Option<Result<BuiltIndex, BuildError>>> =
        (0..at.len()).map(|_| None).collect();
    for (i, r) in at.into_iter().zip(built) {
        out[i] = Some(r);
    }
    out.into_iter().map(Option::unwrap).collect()
}
