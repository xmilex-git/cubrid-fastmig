//! Builds several indexes at the same time under the one worker limit.
//!
//! Input: one job per index. Output: each job's result, in job order.
//! Source: workspace#481 G1 (`--workers` caps every thread; indexes build concurrently, R6 § 2 step 0 notes the
//! engine builds one at a time). Each build holds one permit for its serial steps and borrows free permits for
//! its parallel ones (`WorkerBudget::try_acquire`), so builds never wait on each other's parallel steps.

use crate::build::{BuiltIndex, build_index};
use crate::build_error::BuildError;
use crate::index_shape::IndexShape;
use crate::key_order::KeyOrder;
use crate::ovf_key_file::MakeSink;
use crate::page_sink::PageSink;
use crate::sort_run::Run;
use crate::worker_budget::WorkerBudget;

pub struct IndexJob<'a> {
    pub shape: IndexShape,
    pub order: &'a dyn KeyOrder,
    pub runs: Vec<Run>,
    pub nulls: u64,
    pub sink: &'a dyn PageSink,
    pub make_ovf_key_sink: &'a MakeSink<'a>,
}

pub fn build_indexes(
    jobs: Vec<IndexJob>,
    budget: &WorkerBudget,
) -> Vec<Result<BuiltIndex, BuildError>> {
    std::thread::scope(|s| {
        let handles: Vec<_> = jobs
            .into_iter()
            .map(|j| {
                s.spawn(move || {
                    build_index(
                        &j.shape,
                        j.order,
                        j.runs,
                        j.nulls,
                        j.sink,
                        j.make_ovf_key_sink,
                        budget,
                    )
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("index build panicked"))
            .collect()
    })
}
