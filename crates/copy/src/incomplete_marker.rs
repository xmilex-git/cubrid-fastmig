//! The incomplete marker `<dest>/FASTMIG_INCOMPLETE`: while it exists the target holds an unverified migration.
//! It also lists, one absolute path per line, every file and directory the tool created, so a rerun removes exactly
//! those and starts over (there is no resume).
//!
//! Input: the target directory. Output: a marker that records created paths; [`IncompleteMarker::finish`] removes
//! it once verification passed.
//! Rules: a missing or empty target directory is used as is; a directory holding a marker is cleaned of the listed
//! paths first; any other non-empty directory is refused. The run holds an exclusive lock on the marker until it
//! ends, so a second run on the same target is refused before it cleans or writes anything; the lock goes with the
//! process, so a rerun after a kill takes it. The lock is `flock(2)` (std `File::try_lock`), not an `fcntl` lock: a
//! POSIX lock would be dropped as soon as any other descriptor of the marker in this process is closed.
//! Source: decision G1 workspace#481 (no resume; rerun cleans what the tool made); ADR 0004; 1번 세션 decision on
//! workspace#526 (two runs on one --dest).

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Write};
use std::os::unix::fs::MetadataExt;

use crate::own_blocks::own_bytes;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const MARKER_NAME: &str = "FASTMIG_INCOMPLETE";
/// Prefix of a scratch line in the marker: a path a rerun removes but the end-of-run fsync skips.
const SCRATCH: &str = "scratch\t";

pub struct IncompleteMarker {
    path: PathBuf,
    file: Mutex<File>,
}

impl IncompleteMarker {
    /// Prepares `target_dir` for a fresh run and writes the marker in it.
    pub fn prepare(target_dir: &Path) -> io::Result<Self> {
        let path = target_dir.join(MARKER_NAME);
        let only_marker = |dir: &Path| -> io::Result<bool> {
            for e in fs::read_dir(dir)? {
                if e?.file_name() != MARKER_NAME {
                    return Ok(false);
                }
            }
            Ok(true)
        };
        if target_dir.exists() {
            if !path.exists() && !only_marker(target_dir)? {
                return Err(io::Error::other(format!(
                    "{} is not empty and holds no {MARKER_NAME}; refusing to write into it",
                    target_dir.display()
                )));
            }
        } else {
            fs::create_dir_all(target_dir)?;
        }
        let mut file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(&path)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                return Err(io::Error::other(format!(
                    "another migrate is running on this target ({} is locked)",
                    path.display()
                )));
            }
            Err(TryLockError::Error(e)) => {
                return Err(io::Error::new(
                    e.kind(),
                    format!("{}: cannot lock: {e}", path.display()),
                ));
            }
        }
        let mut listed = String::new();
        file.read_to_string(&mut listed)?;
        remove_listed(&listed)?;
        file.set_len(0)?;
        if !only_marker(target_dir)? {
            return Err(io::Error::other(format!(
                "{} still holds files the previous run did not list; refusing to write into it",
                target_dir.display()
            )));
        }
        Ok(IncompleteMarker {
            path,
            file: Mutex::new(file),
        })
    }

    /// Records `p` (absolute) before the tool creates it. Not fsynced per line: after a crash that lost lines, the
    /// rerun stops at a directory still holding unlisted files (it never removes what it cannot name).
    pub fn record(&self, p: &Path) -> io::Result<()> {
        let s = p.to_str().filter(|s| p.is_absolute() && !s.contains('\n'));
        let s = s.ok_or_else(|| io::Error::other(format!("cannot record path {}", p.display())))?;
        let mut f = self.file.lock().unwrap();
        f.write_all(format!("{s}\n").as_bytes())
    }

    /// Records scratch file `p` (absolute) before the tool creates it: removed by a rerun like any recorded path,
    /// but not in [`created`](Self::created), so the end-of-run fsync never flushes it (the spill arena,
    /// workspace#526).
    pub fn record_scratch(&self, p: &Path) -> io::Result<()> {
        let s = p.to_str().filter(|s| p.is_absolute() && !s.contains('\n'));
        let s = s.ok_or_else(|| io::Error::other(format!("cannot record path {}", p.display())))?;
        let mut f = self.file.lock().unwrap();
        f.write_all(format!("{SCRATCH}{s}\n").as_bytes())
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

    /// Every path recorded so far, in creation order.
    pub fn created(&self) -> io::Result<Vec<PathBuf>> {
        Ok(fs::read_to_string(&self.path)?
            .lines()
            .filter(|l| !l.starts_with(SCRATCH))
            .map(PathBuf::from)
            .collect())
    }

    /// Removes the marker: the migration in this directory passed verification.
    pub fn finish(self) -> io::Result<()> {
        fs::remove_file(&self.path)
    }
}

/// Bytes the files a previous run listed in `target_dir`'s marker hold on `target_dir`'s filesystem and share with
/// no other file (a clone's shared blocks stay with its source): a rerun
/// removes them before it writes, so its free-space check counts them as free (workspace#526: a rerun after a kill
/// on a filesystem the first run filled was refused). 0 without a marker.
pub fn reclaimable_bytes(target_dir: &Path) -> io::Result<u64> {
    let listed = match fs::read_to_string(target_dir.join(MARKER_NAME)) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let dev = fs::metadata(target_dir)?.dev();
    let mut bytes = 0;
    for l in listed.lines() {
        let p = Path::new(l.strip_prefix(SCRATCH).unwrap_or(l));
        let Ok(m) = fs::symlink_metadata(p) else {
            continue;
        };
        if m.is_file() && m.dev() == dev {
            // a cloned volume gives back only the blocks it no longer shares
            bytes += own_bytes(&File::open(p)?)?;
        }
    }
    Ok(bytes)
}

/// Removes the paths a previous run listed, newest first, so directories are empty by the time they are reached.
fn remove_listed(listed: &str) -> io::Result<()> {
    for line in listed.lines().rev() {
        let p = Path::new(line.strip_prefix(SCRATCH).unwrap_or(line));
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
