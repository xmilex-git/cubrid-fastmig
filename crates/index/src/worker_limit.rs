//! The one worker limit of a run (`--workers`), shared by every index built at the same time.
//!
//! Input: the limit. Output: permits a parallel step holds while its threads run.
//! Source: workspace#481 G1 — `--workers N` caps every thread of the process; `--workers 1` is the
//! single-thread baseline.

use std::sync::{Condvar, Mutex};

pub struct WorkerLimit {
    free: Mutex<usize>,
    freed: Condvar,
    limit: usize,
}

/// Permits held; returned to the limit on drop.
pub struct Permits<'a> {
    limit: &'a WorkerLimit,
    pub count: usize,
}

impl WorkerLimit {
    pub fn new(limit: usize) -> Self {
        let limit = limit.max(1);
        Self {
            free: Mutex::new(limit),
            freed: Condvar::new(),
            limit,
        }
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    /// Takes up to `want` free permits without waiting (possibly none). An index build holds one permit from
    /// `acquire` for its whole run and adds these for its parallel steps, so builds never wait on each other.
    pub fn try_acquire(&self, want: usize) -> Permits<'_> {
        let mut free = self.free.lock().unwrap();
        let count = want.min(*free);
        *free -= count;
        Permits { limit: self, count }
    }

    /// Waits for at least one permit, then takes up to `want` of those free.
    pub fn acquire(&self, want: usize) -> Permits<'_> {
        let mut free = self.free.lock().unwrap();
        while *free == 0 {
            free = self.freed.wait(free).unwrap();
        }
        let count = want.clamp(1, *free);
        *free -= count;
        Permits { limit: self, count }
    }
}

impl Drop for Permits<'_> {
    fn drop(&mut self) {
        *self.limit.free.lock().unwrap() += self.count;
        self.limit.freed.notify_all();
    }
}
