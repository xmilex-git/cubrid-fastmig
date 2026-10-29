//! Copies out-of-volume assets: the referenced LOB files listed in the LOB manifest, and the Java SP directories
//! `java` and `java_static`. Each file is cloned when the filesystem allows it and byte-copied otherwise, on the
//! shared worker cap; nothing is converted.
//!
//! Input: source and target roots, the file list (manifest entries or a directory walk), the worker cap and the
//! incomplete marker. Output: counts per copy method, and the manifest entries whose source file is missing (a
//! pre-existing defect: skipped and reported, not an error).
//! Source: decision G6 workspace#488 (copy referenced files only, clone first; Java directories copied as is);
//! ADR 0009.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

use crate::clone_file::try_clone;
use crate::incomplete_marker::IncompleteMarker;
use crate::lob_manifest::LobEntry;
use crate::volume_copy::CopyMethod;
use crate::worker_pool;

#[derive(Debug, Default)]
pub struct AssetOutcome {
    pub cloned: u64,
    pub copied: u64,
    pub bytes: u64,
    /// Listed files absent in the source (relative paths).
    pub missing: Vec<PathBuf>,
}

/// Copies the manifest's files from `src_base` to `dst_base`, keeping each relative path.
pub fn copy_lob_files(
    src_base: &Path,
    dst_base: &Path,
    entries: &[LobEntry],
    workers: usize,
    marker: &IncompleteMarker,
) -> io::Result<AssetOutcome> {
    for e in entries {
        // a locator path must stay below the base (it is joined to both roots)
        if !e
            .rel_path
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
        {
            return Err(io::Error::other(format!(
                "LOB path leaves the base: {}",
                e.rel_path.display()
            )));
        }
        if let Some(parent) = e.rel_path.parent() {
            marker.create_dir_all(&dst_base.join(parent))?;
        }
    }
    marker.create_dir_all(dst_base)?;
    let files: Vec<PathBuf> = entries.iter().map(|e| e.rel_path.clone()).collect();
    copy_files(src_base, dst_base, &files, workers, marker, true)
}

/// Copies directory `src` to `dst` with everything below it; a missing `src` copies nothing.
pub fn copy_dir_tree(
    src: &Path,
    dst: &Path,
    workers: usize,
    marker: &IncompleteMarker,
) -> io::Result<AssetOutcome> {
    if !src.is_dir() {
        return Ok(AssetOutcome::default());
    }
    let mut files = Vec::new();
    walk(src, Path::new(""), dst, marker, &mut files)?;
    copy_files(src, dst, &files, workers, marker, false)
}

fn walk(
    root: &Path,
    rel: &Path,
    dst_root: &Path,
    marker: &IncompleteMarker,
    files: &mut Vec<PathBuf>,
) -> io::Result<()> {
    marker.create_dir_all(&dst_root.join(rel))?;
    for ent in fs::read_dir(root.join(rel))? {
        let ent = ent?;
        let rel_child = rel.join(ent.file_name());
        let ty = ent.file_type()?;
        if ty.is_dir() {
            walk(root, &rel_child, dst_root, marker, files)?;
        } else if ty.is_file() {
            files.push(rel_child);
        } else {
            return Err(io::Error::other(format!(
                "not a file or directory: {}",
                ent.path().display()
            )));
        }
    }
    Ok(())
}

fn copy_files(
    src_root: &Path,
    dst_root: &Path,
    files: &[PathBuf],
    workers: usize,
    marker: &IncompleteMarker,
    missing_ok: bool,
) -> io::Result<AssetOutcome> {
    let out = Mutex::new(AssetOutcome::default());
    worker_pool::run(files.len(), workers, |i| {
        let src = src_root.join(&files[i]);
        let src_file = match File::open(&src) {
            Ok(f) => f,
            Err(e) if missing_ok && e.kind() == io::ErrorKind::NotFound => {
                out.lock().unwrap().missing.push(files[i].clone());
                return Ok(());
            }
            Err(e) => return Err(io::Error::new(e.kind(), format!("{}: {e}", src.display()))),
        };
        let (method, bytes) = copy_file(&src_file, &dst_root.join(&files[i]), marker)?;
        let mut o = out.lock().unwrap();
        match method {
            CopyMethod::Clone => o.cloned += 1,
            CopyMethod::Copy => o.copied += 1,
        }
        o.bytes += bytes;
        Ok(())
    })?;
    Ok(out.into_inner().unwrap())
}

/// Clones or copies one file to a new `dst` with the source's permission bits.
fn copy_file(src: &File, dst: &Path, marker: &IncompleteMarker) -> io::Result<(CopyMethod, u64)> {
    let meta = src.metadata()?;
    marker.record(dst)?;
    let mut out = OpenOptions::new().write(true).create_new(true).open(dst)?;
    out.set_permissions(meta.permissions())?;
    let method = if try_clone(src, &out)? {
        CopyMethod::Clone
    } else {
        io::copy(&mut &*src, &mut out)?;
        CopyMethod::Copy
    };
    out.sync_all()?;
    Ok((method, meta.len()))
}
