//! Runs N independent tasks on at most `workers` threads, each thread taking the next task as it finishes one
//! (a sliding window: 64 volumes with 4 workers run 4 at a time).
//!
//! Input: the task count, the thread cap and a task function. Output: the first error any task returned; once one
//! fails, threads stop taking new tasks.
//! Source: decision G1 workspace#481 (`--workers` is the one process-wide thread cap).

use std::io;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

pub fn run<F>(n_tasks: usize, workers: usize, task: F) -> io::Result<()>
where
    F: Fn(usize) -> io::Result<()> + Sync,
{
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let first_err: Mutex<Option<io::Error>> = Mutex::new(None);
    std::thread::scope(|s| {
        for _ in 0..workers.clamp(1, n_tasks.max(1)) {
            s.spawn(|| {
                while !failed.load(Ordering::Relaxed) {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= n_tasks {
                        break;
                    }
                    if let Err(e) = task(i) {
                        failed.store(true, Ordering::Relaxed);
                        first_err.lock().unwrap().get_or_insert(e);
                    }
                }
            });
        }
    });
    match first_err.into_inner().unwrap() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}
