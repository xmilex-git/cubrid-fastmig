//! Makes everything the tool created durable in one pass at the end of the run, instead of one fsync per write
//! step: every file and directory listed in the incomplete marker, and the directories holding them, are fsynced on
//! the shared worker cap.
//!
//! Input: the incomplete marker and the worker cap. Output: all created files and directories flushed to disk.
//! Must run before the target is registered, so a crash never leaves a registered target with unwritten pages.
//! Policy: this pass is the run's only fsync, and it covers exactly what the marker lists — the target's files.
//! Spill and scratch files (`--spill-dir`, sort runs, verifier merge files) are never listed and never fsynced:
//! they die with the run, and a crash restarts from scratch anyway (no resume), as CBRD-27626 does for temp pages.
//! A cloned volume stays in the pass although none of its data was written: the reflink itself is a metadata
//! change, and its fsync is a log force (about 13-34 ms per run measured, workspace#494).
//! Source: Linux `fsync(2)` (flushes the inode's dirty data whichever descriptor calls it, and a directory's own
//! fsync makes its new entries durable).

use std::collections::BTreeSet;
use std::fs::File;
use std::io;
use std::path::PathBuf;

use crate::incomplete_marker::IncompleteMarker;
use crate::worker_pool;

pub fn sync_created(marker: &IncompleteMarker, workers: usize) -> io::Result<()> {
    // parents too: a directory the tool did not create (the target dir itself) still gained entries
    let mut set = BTreeSet::new();
    for p in marker.created()? {
        set.extend(p.parent().map(PathBuf::from));
        set.insert(p);
    }
    let paths: Vec<PathBuf> = set.into_iter().collect();
    worker_pool::run(paths.len(), workers, |i| match File::open(&paths[i]) {
        Ok(f) => f.sync_all(),
        // a listed path the run did not get to create (create failed after recording) has nothing to flush
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io::Error::new(
            e.kind(),
            format!("{}: {e}", paths[i].display()),
        )),
    })
}
