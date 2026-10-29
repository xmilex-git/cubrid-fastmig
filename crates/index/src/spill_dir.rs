//! The directory sorted runs spill to when keys outgrow their memory grant, and its cleanup.
//!
//! Input: the base (cli `Limits::spill_dir`, default `<dest>/fastmig/spill` on the target's filesystem, never
//! `/tmp`). Output: a private subdirectory and fresh file paths in it; on drop every file and the subdirectory are
//! removed, on success and on failure alike. A killed process leaves them inside the target, which a rerun
//! rebuilds from scratch (workspace#481 incomplete marker).
//! Source: workspace#493.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::spill_arena::SpillArena;

pub struct SpillDir {
    dir: PathBuf,
    next: AtomicU64,
    /// With a temp disk limit every run goes into this arena and no file is created here (workspace#526).
    arena: Option<Arc<SpillArena>>,
}

impl SpillDir {
    /// Runs written into `arena`: nothing is created on disk (the arena is already allocated).
    pub fn in_arena(arena: Arc<SpillArena>) -> Self {
        Self {
            dir: arena.path().to_path_buf(),
            next: AtomicU64::new(0),
            arena: Some(arena),
        }
    }

    pub fn arena(&self) -> Option<&Arc<SpillArena>> {
        self.arena.as_ref()
    }

    pub fn create(base: &Path) -> io::Result<Self> {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        std::fs::create_dir_all(base)?;
        let dir = base.join(format!(
            "index-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir)?;
        Ok(Self {
            dir,
            next: AtomicU64::new(0),
            arena: None,
        })
    }

    pub fn path(&self) -> &Path {
        &self.dir
    }

    pub fn new_file_path(&self) -> PathBuf {
        self.dir.join(format!(
            "run-{}.bin",
            self.next.fetch_add(1, Ordering::Relaxed)
        ))
    }
}

impl Drop for SpillDir {
    fn drop(&mut self) {
        if self.arena.is_some() {
            return;
        }
        if let Ok(entries) = std::fs::read_dir(&self.dir) {
            for e in entries.flatten() {
                let _ = std::fs::remove_file(e.path());
            }
        }
        let _ = std::fs::remove_dir(&self.dir);
    }
}
