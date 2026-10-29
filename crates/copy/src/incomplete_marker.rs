//! The incomplete marker `<dest>/FASTMIG_INCOMPLETE`: while it exists the target holds an unverified migration.
//! It also lists, one absolute path per line, every file and directory the tool created, so a rerun removes exactly
//! those and starts over (there is no resume).
//!
//! Input: the target directory. Output: a marker that records created paths; [`IncompleteMarker::finish`] removes
//! it once verification passed.
//! Rules: a missing or empty target directory is used as is; a directory holding a marker is cleaned of the listed
//! paths first; any other non-empty directory is refused.
//! Source: decision G1 workspace#481 (no resume; rerun cleans what the tool made); ADR 0004.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const MARKER_NAME: &str = "FASTMIG_INCOMPLETE";

pub struct IncompleteMarker {
    path: PathBuf,
    file: Mutex<File>,
}

impl IncompleteMarker {
    /// Prepares `dest` for a fresh run and writes the marker in it.
    pub fn prepare(dest: &Path) -> io::Result<Self> {
        let path = dest.join(MARKER_NAME);
        if path.exists() {
            remove_listed(&path)?;
            fs::remove_file(&path)?;
        }
        if dest.exists() {
            if fs::read_dir(dest)?.next().is_some() {
                return Err(io::Error::other(format!(
                    "{} is not empty and holds no {MARKER_NAME}; refusing to write into it",
                    dest.display()
                )));
            }
        } else {
            fs::create_dir_all(dest)?;
        }
        let file = OpenOptions::new()
            .append(true)
            .create_new(true)
            .open(&path)?;
        Ok(IncompleteMarker {
            path,
            file: Mutex::new(file),
        })
    }

    /// Records `p` (absolute) before the tool creates it, so a crash right after still leaves it listed.
    pub fn record(&self, p: &Path) -> io::Result<()> {
        let s = p.to_str().filter(|s| p.is_absolute() && !s.contains('\n'));
        let s = s.ok_or_else(|| io::Error::other(format!("cannot record path {}", p.display())))?;
        let mut f = self.file.lock().unwrap();
        f.write_all(format!("{s}\n").as_bytes())?;
        f.sync_data()
    }

    /// Creates directory `p` and its missing parents, recording each one it creates.
    pub fn create_dir_all(&self, p: &Path) -> io::Result<()> {
        if p.is_dir() {
            return Ok(());
        }
        if let Some(parent) = p.parent() {
            self.create_dir_all(parent)?;
        }
        self.record(p)?;
        fs::create_dir(p)
    }

    /// Removes the marker: the migration in this directory passed verification.
    pub fn finish(self) -> io::Result<()> {
        fs::remove_file(&self.path)
    }
}

/// Removes the paths a previous run listed, newest first, so directories are empty by the time they are reached.
fn remove_listed(marker: &Path) -> io::Result<()> {
    let listed = fs::read_to_string(marker)?;
    for line in listed.lines().rev() {
        let p = Path::new(line);
        let removed = match fs::symlink_metadata(p) {
            Ok(m) if m.is_dir() => fs::remove_dir(p),
            Ok(_) => fs::remove_file(p),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        };
        // a directory holding files the tool did not create is left for the user to clear
        removed.map_err(|e| {
            io::Error::new(e.kind(), format!("cleaning the previous run: {line}: {e}"))
        })?;
    }
    Ok(())
}
