//! The progress of a pass that waits for writes to reach the devices (an fsync pass), counted as the devices write:
//! one scale for the whole pass, so a slow fsync does not leave the phase's counter standing still (workspace#606).
//!
//! Input: the phase's meter, the pass's total (the bytes the run wrote that the pass makes durable: its scale) and the
//! files' total length; the pass calls [`SyncProgress::file_synced`] as each file's sync returns. Output: the meter
//! raised, never past the total, never down.
//! Rule (workspace#606 plan A, 1번 세션 2026-11-01): at the start the system has D0 bytes dirty or under writeback
//! (`dirty_bytes`); the pass's dirty share is min(D0, total) and its clean share the rest (written back already). The
//! clean share counts at once; the dirty share as the system's dirty bytes fall: count = clean + dirty share x
//! (D0 - now) / D0, read every [`WATCH`]; the pass's end counts the rest. The synced files' lengths count only when
//! there is no watcher: a clean file's length is not bytes written, and mixing the two scales held the count still.
//! Edge cases:
//! - D0 = 0: everything is clean and counts at once;
//! - D0 > total (other processes' dirty pages): the dirty share is the total and its count runs slow, erring long;
//! - pages dirtied during the pass, by anyone, raise "now": the count stands still for a while, never goes down;
//! - `/proc/meminfo` unreadable: the synced files' share of their length, times the total, counts, as before
//!   workspace#606.
//!
//! Three earlier versions failed on the dev host HDD (counter idle 8 s, 4 s, 3 s): counting section completions (they wait
//! behind the kernel flusher's writes), spreading the device progress over the files' whole length (it overlapped the
//! clean files counted at once), and the files' share as a floor of the bytes written. Kept to one scale here.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::time::Duration;

use fastmig_progress::Meter;

use crate::dirty_bytes;

/// How often the watcher reads the system's dirty bytes.
pub const WATCH: Duration = Duration::from_millis(250);

pub struct SyncProgress<'a> {
    meter: &'a Meter<'a>,
    total: u64,
    /// What `meter` has been given.
    shown: AtomicU64,
    /// Lengths of the files whose sync returned, of `files_total`.
    files: AtomicU64,
    files_total: u64,
    /// `/proc/meminfo` was read at the start: the watcher counts, the files do not.
    watched: bool,
}

impl<'a> SyncProgress<'a> {
    /// Runs `pass` with a watcher counting `total` bytes on `meter` until `pass` returns; the files `pass` syncs are
    /// `files_total` bytes long.
    pub fn run<R>(
        meter: &'a Meter<'a>,
        total: u64,
        files_total: u64,
        pass: impl FnOnce(&SyncProgress<'a>) -> R,
    ) -> R {
        let d0 = dirty_bytes::system();
        let p = SyncProgress {
            meter,
            total,
            shown: AtomicU64::new(0),
            files: AtomicU64::new(0),
            files_total,
            watched: d0.is_some(),
        };
        let dirty_share = d0.map(|d| d.min(total));
        if let Some(dirty) = dirty_share {
            p.raise(total - dirty);
        }
        let done = AtomicBool::new(false);
        std::thread::scope(|s| {
            let watcher = s.spawn(|| match (d0, dirty_share) {
                (Some(d0), Some(dirty)) if dirty > 0 => p.watch(d0, dirty, &done),
                _ => {}
            });
            let r = pass(&p);
            done.store(true, Relaxed);
            watcher.thread().unpark();
            p.raise(p.total);
            r
        })
    }

    /// One file's sync returned: without a watcher, its length counts.
    pub fn file_synced(&self, len: u64) {
        if self.watched {
            return;
        }
        let files = self.files.fetch_add(len, Relaxed) + len;
        let share = files as f64 / self.files_total.max(1) as f64;
        self.raise((share * self.total as f64) as u64);
    }

    fn watch(&self, d0: u64, dirty: u64, done: &AtomicBool) {
        let clean = self.total - dirty;
        while !done.load(Relaxed) {
            std::thread::park_timeout(WATCH);
            let Some(now) = dirty_bytes::system() else {
                return;
            };
            let share = d0.saturating_sub(now) as f64 / d0 as f64;
            self.raise(clean + (share * dirty as f64) as u64);
        }
    }

    /// Gives `meter` what `to`, capped at the total, adds to the count so far.
    fn raise(&self, to: u64) {
        let to = to.min(self.total);
        let prev = self.shown.fetch_max(to, Relaxed);
        if to > prev {
            self.meter.add(to - prev);
        }
    }
}
