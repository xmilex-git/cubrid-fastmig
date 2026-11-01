//! `-r, --replace`: a database already registered under the target name in the target `databases.txt` is removed
//! before the migration writes, over the range `cubrid deletedb` removes: its volumes (the `_vinf` volume lines), its
//! log directory's files (`_lgat`, `_lgar*`, `_lginf`, `_vinf`, `_dwb`) and its `databases.txt` line; and when it is a
//! finished fastmig target, every path its record `<vol-path>/fastmig/created-paths` lists (the files and directories
//! that migrate created: `fastmig/`, the referenced LOB files and their directories, `java/`, `java_static/`,
//! `<db>_keys`, ...). The record counts only when it lists the database's first volume.
//!
//! Input: the target `databases.txt`, the target name, the source's volumes, the target directory and the target
//! volume paths. Output: the database to replace (or none: the name is free), and refusals the plan prints:
//! its server is up (its active log is locked, as for the source), one of its volumes is a source volume, it has no
//! `_vinf` to list its volumes by, or the target directory would still hold other files once it is removed (the
//! incomplete marker refuses a non-empty directory; checked before anything is deleted, so a refused run leaves the
//! old database whole). A listed directory still holding files nobody listed (outside the target directory) is left.
//! Removal happens in migrate after the plan passed, with the old log read-locked.
//! Source: decision G13 workspace#604 (copydb `-r`: deletedb's range, refuse while its server is up); engine
//! `boot_sr.c:xboot_delete`, `boot_remove_all_volumes` (develop 5b1867174); user decision (b) on workspace#605,
//! relayed by the 1번 session 2026-11-01 (also the paths of the fastmig record; other files left in the target
//! directory refuse, named in the error).

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use fastmig_copy::incomplete_marker::{CREATED_PATHS, MARKER_NAME};
use fastmig_copy::open_locked_log;
use fastmig_ondisk::databases_txt::{DatabaseEntry, find};

use crate::database_files::{log_files, remove_file, unregister, vinf_volumes};

/// The registered database `--replace` removes.
pub struct ReplacedDatabase {
    pub entry: DatabaseEntry,
    pub volumes: Vec<PathBuf>,
    pub logs: Vec<PathBuf>,
    /// The paths of its fastmig record, in creation order (empty: no record that names it).
    pub created: Vec<PathBuf>,
}

impl ReplacedDatabase {
    fn lgat(&self) -> PathBuf {
        self.entry
            .log_path
            .join(format!("{}_lgat", self.entry.name))
    }

    fn paths(&self) -> impl Iterator<Item = &PathBuf> {
        self.volumes.iter().chain(&self.logs).chain(&self.created)
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// The paths of `<vol-path>/fastmig/created-paths` when it lists the first volume of `entry`.
fn fastmig_record(entry: &DatabaseEntry) -> io::Result<Vec<PathBuf>> {
    let text = match fs::read_to_string(entry.vol_path.join("fastmig").join(CREATED_PATHS)) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let created: Vec<PathBuf> = text
        .lines()
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .collect();
    let first = entry.first_volume();
    Ok(if created.iter().any(|p| same_file(p, &first)) {
        created
    } else {
        Vec::new()
    })
}

/// Files below `dir` (relative names) that are not in `removed` and lie in no directory that is.
fn left_over(
    dir: &Path,
    removed: &HashSet<PathBuf>,
    root: &Path,
    out: &mut Vec<String>,
) -> io::Result<()> {
    for e in fs::read_dir(dir)? {
        let e = e?;
        let p = e.path();
        if removed.contains(&p) && !e.file_type()?.is_dir() {
            continue;
        }
        if e.file_type()?.is_dir() {
            // a directory goes only once everything below it goes
            left_over(&p, removed, root, out)?;
            if !removed.contains(&p) {
                out.push(format!("{}/", p.strip_prefix(root).unwrap_or(&p).display()));
            }
        } else {
            out.push(p.strip_prefix(root).unwrap_or(&p).display().to_string());
        }
    }
    Ok(())
}

/// The database registered as `name` in `dbtxt` and why it cannot be replaced (empty: it can).
pub fn find_replaced(
    dbtxt: &Path,
    name: &str,
    source_volumes: &[PathBuf],
    target_dir: &Path,
    volume_targets: &[PathBuf],
) -> io::Result<(Option<ReplacedDatabase>, Vec<String>)> {
    let text = match fs::read_to_string(dbtxt) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok((None, Vec::new())),
        Err(e) => return Err(e),
    };
    let Some(entry) = find(&text, name) else {
        return Ok((None, Vec::new()));
    };
    let mut refusals = Vec::new();
    let vinf = entry.log_path.join(format!("{name}_vinf"));
    let volumes: Vec<PathBuf> = vinf_volumes(&vinf).into_iter().map(|(_, p)| p).collect();
    if volumes.is_empty() {
        refusals.push(format!(
            "--replace: {name} in {} has no readable {} to list its volumes by; delete it with cubrid deletedb",
            dbtxt.display(),
            vinf.display()
        ));
    }
    let logs = if entry.log_path.is_dir() {
        log_files(&entry.log_path, name)?
    } else {
        Vec::new()
    };
    let created = fastmig_record(&entry)?;
    let r = ReplacedDatabase {
        entry,
        volumes,
        logs,
        created,
    };
    if let Some(v) = r
        .paths()
        .find(|v| source_volumes.iter().any(|s| same_file(v, s)))
    {
        refusals.push(format!(
            "--replace: {} of {name} is a file of the source; the source is never replaced",
            v.display()
        ));
    }
    let lgat = r.lgat();
    if lgat.exists()
        && let Err(e) = open_locked_log(&lgat)
    {
        refusals.push(format!(
            "--replace: {name} cannot be replaced while its server is up ({e})"
        ));
    }
    // what stays in the target directory, and target volumes that exist, once the old database is gone
    let removed: HashSet<PathBuf> = r
        .paths()
        .map(|p| fs::canonicalize(p).unwrap_or_else(|_| p.clone()))
        .collect();
    if target_dir.is_dir() && !target_dir.join(MARKER_NAME).exists() {
        let root = fs::canonicalize(target_dir)?;
        let mut left = Vec::new();
        left_over(&root, &removed, &root, &mut left)?;
        if !left.is_empty() {
            left.sort();
            refusals.push(format!(
                "--replace: {} would still hold files {name}'s record does not list once it is removed: {}",
                target_dir.display(),
                left.join(", ")
            ));
        }
    }
    for t in volume_targets.iter().filter(|t| !t.starts_with(target_dir)) {
        if t.exists() && !r.paths().any(|q| same_file(q, t)) {
            refusals.push(format!("target volume {} already exists", t.display()));
        }
    }
    Ok((Some(r), refusals))
}

/// Removes the replaced database, its log read-locked while the files go; returns the paths removed. The record's
/// paths go newest first, so a directory is empty when it is reached; one that is not stays.
pub fn remove(r: &ReplacedDatabase, dbtxt: &Path) -> io::Result<Vec<PathBuf>> {
    let lgat = r.lgat();
    let _lock = if lgat.exists() {
        Some(open_locked_log(&lgat)?)
    } else {
        None
    };
    let mut deleted = Vec::new();
    for p in r.volumes.iter().chain(&r.logs) {
        remove_file(p, &mut deleted)?;
    }
    for p in r.created.iter().rev() {
        match fs::symlink_metadata(p) {
            Ok(m) if m.is_dir() => {
                if fs::read_dir(p)?.next().is_none() {
                    fs::remove_dir(p)?;
                    deleted.push(p.clone());
                }
            }
            Ok(_) => remove_file(p, &mut deleted)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(io::Error::new(e.kind(), format!("{}: {e}", p.display()))),
        }
    }
    unregister(dbtxt, &r.entry.name)?;
    Ok(deleted)
}
