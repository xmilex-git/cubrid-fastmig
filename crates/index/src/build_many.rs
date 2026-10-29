//! Builds several indexes at the same time under the one worker limit.
//!
//! Input: one job per index. Output: each job's result, in job order.
//! Source: workspace#481 G1 (`--workers` caps every thread; indexes build concurrently, R6 § 2 step 0 notes the
//! engine builds one at a time). Each build holds one permit for its serial steps and borrows free permits for
//! its parallel ones (`WorkerBudget::try_acquire`), so builds never wait on each other's parallel steps. Their keys
//! are already in memory grants or spill files (`IndexRuns`); a build only borrows merge buffers on top.

use crate::build::{BuiltIndex, IndexJob, build_index};
use crate::build_error::BuildError;
use crate::memory_budget::MemoryBudget;
use crate::worker_budget::WorkerBudget;

pub fn build_indexes(
    jobs: Vec<IndexJob>,
    budget: &WorkerBudget,
    memory: &MemoryBudget,
) -> Vec<Result<BuiltIndex, BuildError>> {
    std::thread::scope(|s| {
        let handles: Vec<_> = jobs
            .into_iter()
            .map(|j| s.spawn(move || build_index(j, budget, memory)))
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("index build panicked"))
            .collect()
    })
}
