//! Makes everything the tool created durable in one pass at the end of the run, instead of one fsync per write
//! step: every file and directory listed in the incomplete marker, and the directories holding them, are fsynced on
//! the shared worker limit.
//!
//! Input: the incomplete marker and the worker limit. Output: all created files and directories flushed to disk.
//! Progress: `meter` (the durable-sync phase) counts the lengths of the files synced so far, or, when that is more,
//! the pass's total length times the share of the system's dirty and writeback bytes written since the pass began
//! (`dirty_bytes`, read every [`WATCH`]): one fsync of a large volume on a slow disk waits seconds behind the kernel
//! flusher's writes, and the devices' progress shows meanwhile (workspace#606: the counter stood 13 s on the dev host
//! HDD). The count only grows and never passes the total.
//! Must run before the target is registered, so a crash never leaves a registered target with unwritten pages.
//! Policy: this pass is the run's only fsync, and it covers exactly what the marker lists — the target's files.
//! Spill and scratch files (`--spill-dir`, sort runs, verifier merge files) are never listed and never fsynced:
//! they die with the run, and a crash restarts from scratch anyway (no resume), as CBRD-27626 does for temp pages.
//! A cloned volume stays in the pass although none of its data was written: the reflink itself is a metadata
//! change, and its fsync is a log force (about 13-34 ms per run measured, workspace#494).
//! Source: Linux `fsync(2)` (flushes the inode's dirty data whichever descriptor calls it, and a directory's own
//! fsync makes its new entries durable).

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::time::Duration;

use fastmig_progress::Meter;

use crate::dirty_bytes;
use crate::incomplete_marker::IncompleteMarker;
use crate::worker_pool;

/// How often the watcher reads the system's dirty bytes.
const WATCH: Duration = Duration::from_millis(250);

pub fn sync_created(
    marker: &IncompleteMarker,
    worker_limit: usize,
    meter: Meter,
) -> io::Result<()> {
    // parents too: a directory the tool did not create (the target dir itself) still gained entries
    let mut set = BTreeSet::new();
    for p in marker.created()? {
        set.extend(p.parent().map(PathBuf::from));
        set.insert(p);
    }
    let paths: Vec<PathBuf> = set.into_iter().collect();
    let count = Count {
        meter: &meter,
        shown: AtomicU64::new(0),
        files: AtomicU64::new(0),
        total: paths
            .iter()
            .filter_map(|p| fs::metadata(p).ok())
            .map(|m| m.len())
            .sum(),
    };
    let done = AtomicBool::new(false);
    std::thread::scope(|s| {
        let watcher = s.spawn(|| count.watch(&done));
        let r = worker_pool::run(paths.len(), worker_limit, |i| match File::open(&paths[i]) {
            Ok(f) => {
                f.sync_all()?;
                count.file_synced(f.metadata()?.len());
                Ok(())
            }
            // a listed path the run did not get to create (create failed after recording) has nothing to flush
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io::Error::new(
                e.kind(),
                format!("{}: {e}", paths[i].display()),
            )),
        });
        done.store(true, Relaxed);
        watcher.thread().unpark();
        r
    })
}

/// The durable-sync phase's count: the larger of the synced files' lengths and the devices' progress.
struct Count<'a> {
    meter: &'a Meter<'a>,
    /// What `meter` has been given.
    shown: AtomicU64,
    /// Lengths of the files whose fsync returned.
    files: AtomicU64,
    total: u64,
}

impl Count<'_> {
    fn file_synced(&self, len: u64) {
        let files = self.files.fetch_add(len, Relaxed) + len;
        self.raise(files);
    }

    /// Until `done`: the share of the system's dirty and writeback bytes at the start written since, times the total.
    /// Nothing when `/proc/meminfo` is unreadable or nothing was dirty.
    fn watch(&self, done: &AtomicBool) {
        let Some(d0) = dirty_bytes::system().filter(|&d| d > 0) else {
            return;
        };
        while !done.load(Relaxed) {
            std::thread::park_timeout(WATCH);
            let Some(d) = dirty_bytes::system() else {
                return;
            };
            let share = d0.saturating_sub(d) as f64 / d0 as f64;
            self.raise((share * self.total as f64) as u64);
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
