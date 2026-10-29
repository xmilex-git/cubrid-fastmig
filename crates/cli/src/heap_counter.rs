//! Counts the process's heap bytes: a global allocator over std's `System` that keeps the current and the peak
//! live heap, for the progress display and the run report.
//!
//! Input: every allocation and free. Output: [`now_and_peak`]. Each thread accumulates its net bytes in a
//! thread-local and folds them into the shared counters only when they pass [`FLUSH_BYTES`] either way, so the
//! allocation path takes no lock and makes no system call; both values are therefore off by up to
//! `FLUSH_BYTES` per thread, and the peak is only checked at a fold.
//! Built with the `heap-counter` feature (default); without it the binary uses `System` directly and the display
//! shows `heap n/a`.
//! Source: user request relayed in workspace#472 (할당 카운터, 추가 3: thread-local, fold per batch of bytes).

// without the feature only `now_and_peak` is reachable
#![cfg_attr(not(feature = "heap-counter"), allow(dead_code))]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicI64, Ordering::Relaxed};

// 64 KiB, not the 1 MiB first asked: two 256 KiB copy buffers stayed below 1 MiB and showed a 0 B heap
const FLUSH_BYTES: i64 = 64 << 10;

static CURRENT: AtomicI64 = AtomicI64::new(0);
static PEAK: AtomicI64 = AtomicI64::new(0);

thread_local! {
    // const, no destructor: usable inside the allocator without allocating or registering anything
    static PENDING: Cell<i64> = const { Cell::new(0) };
}

pub struct Counting;

#[cfg(feature = "heap-counter")]
#[global_allocator]
static ALLOC: Counting = Counting;

fn note(delta: i64) {
    // a thread being torn down has no thread-local any more; its last bytes go uncounted
    let _ = PENDING.try_with(|p| {
        let v = p.get() + delta;
        if v.abs() >= FLUSH_BYTES {
            let cur = CURRENT.fetch_add(v, Relaxed) + v;
            PEAK.fetch_max(cur, Relaxed);
            p.set(0);
        } else {
            p.set(v);
        }
    });
}

// SAFETY: every call forwards to `System` unchanged; the counting touches only atomics and a const thread-local.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        note(l.size() as i64);
        unsafe { System.alloc(l) }
    }

    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        note(l.size() as i64);
        unsafe { System.alloc_zeroed(l) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, l: Layout) {
        note(-(l.size() as i64));
        unsafe { System.dealloc(ptr, l) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, l: Layout, new_size: usize) -> *mut u8 {
        note(new_size as i64 - l.size() as i64);
        unsafe { System.realloc(ptr, l, new_size) }
    }
}

/// Current and peak heap bytes; `None` in a build without the counter.
pub fn now_and_peak() -> Option<(u64, u64)> {
    cfg!(feature = "heap-counter").then(|| {
        (
            CURRENT.load(Relaxed).max(0) as u64,
            PEAK.load(Relaxed).max(0) as u64,
        )
    })
}
