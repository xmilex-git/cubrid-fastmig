//! Runs a list of jobs on a fixed number of threads: each thread takes the next job until none is left.
//!
//! Input: the jobs in the order they should start (callers put the largest first, LPT, so the longest job does
//! not start last and stretch the tail) and the thread count. Output: each job's result, in job order.
//! Source: workspace#503 (no thread per index: two thousand small indexes run on `--workers` threads, G1
//! workspace#481 caps every thread with that one limit).

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

pub fn run_jobs<T: Send, R: Send>(
    threads: usize,
    jobs: Vec<T>,
    run: impl Fn(T) -> R + Sync,
) -> Vec<R> {
    let n = jobs.len();
    let jobs: Vec<Mutex<Option<T>>> = jobs.into_iter().map(|j| Mutex::new(Some(j))).collect();
    let results: Vec<Mutex<Option<R>>> = (0..n).map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..threads.clamp(1, n.max(1)) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= n {
                        break;
                    }
                    let job = jobs[i].lock().unwrap().take().unwrap();
                    *results[i].lock().unwrap() = Some(run(job));
                }
            });
        }
    });
    results
        .into_iter()
        .map(|r| r.into_inner().unwrap().unwrap())
        .collect()
}
