//! The copy of the source that the source engine unloads for the data comparison: `unloaddb -S` writes to the
//! database it reads (volume 0 and the active log changed on a develop baseline copy, workspace#611 scratch/611 p4),
//! and the source must stay as migrate fingerprinted it until finalize, so the engine never opens the source itself.
//!
//! Input: the source fingerprint (volumes, log directory, name), a new directory under `<dest>/fastmig/post/`.
//! Output: the directory with every volume, `_lgat`, `_lginf`, `_keys` (when present), a `_vinf` naming the copies,
//! a `databases.txt` registering the copy there (lob-base an empty `lob/` of the copy: a develop engine removes
//! `ces_*` directories of the lob-base at boot, CBRD-23700); archives and `_dwb` are not copied.
//! Space: a file on another filesystem than the directory is copied byte by byte; the copy is refused before its first
//! write when those bytes exceed the free space there. Files are reflink clones where the filesystem allows it.
//! The volume headers keep the source's paths: the engine finds volumes through `_vinf` first and only notes a header
//! path that differs (basedb/safe_copy.sh, engine `boot_sr.c:boot_find_rest_permanent_volumes`).
//! Source: workspace#611 decision 3 (e) and the 1번 session's addition 2026-11-02 (space check, nothing left behind).

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use fastmig_copy::clone_file::try_clone;
use fastmig_plan::copy_method::device_of;
use fastmig_plan::free_space::available_bytes;

use crate::source_fingerprint::SourceFingerprint;

/// Not enough free space for the byte copies: (bytes needed, bytes free, directory).
pub struct NoSpace(pub u64, pub u64, pub PathBuf);

/// Every file the copy takes: (source path, file name in the copy).
fn files(f: &SourceFingerprint) -> Vec<(PathBuf, String)> {
    let mut out: Vec<(PathBuf, String)> = f
        .volumes
        .iter()
        .filter_map(|(_, p)| Some((p.clone(), p.file_name()?.to_string_lossy().into_owned())))
        .collect();
    for suffix in ["_lgat", "_lginf", "_keys"] {
        let name = format!("{}{suffix}", f.db);
        let p = f.log_path.join(&name);
        // _keys may also sit beside volume 0
        let p = if p.exists() {
            p
        } else {
            f.vol_path.join(&name)
        };
        if p.exists() {
            out.push((p, name));
        }
    }
    out
}

/// Bytes the copy into `dir` must write (files on another filesystem), and the free bytes of `dir`'s filesystem.
pub fn space(f: &SourceFingerprint, dir: &Path) -> io::Result<(u64, u64)> {
    let dev = device_of(dir)?;
    let mut need = 0;
    for (p, _) in files(f) {
        if device_of(&p)? != dev {
            need += fs::metadata(&p)?.len();
        }
    }
    Ok((need, available_bytes(dir)?))
}

/// Makes the copy in `dir` (created here; its parent exists). Returns the copy's databases.txt.
pub fn make(f: &SourceFingerprint, dir: &Path) -> io::Result<Result<PathBuf, NoSpace>> {
    let parent = dir.parent().unwrap_or(Path::new("."));
    let (need, free) = space(f, parent)?;
    if need > free {
        return Ok(Err(NoSpace(need, free, parent.to_path_buf())));
    }
    fs::create_dir(dir)?;
    fs::create_dir(dir.join("lob"))?;
    for (src, name) in files(f) {
        let from = File::open(&src)?;
        let to = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dir.join(&name))?;
        if !try_clone(&from, &to)? {
            io::copy(&mut &from, &mut &to)?;
        }
    }
    // "%4d %s" per line, as the engine writes it (log_page_buffer.c logpb_add_volume)
    let vinf = f.log_path.join(format!("{}_vinf", f.db));
    let mut text = String::new();
    for l in fs::read_to_string(&vinf)?.lines() {
        let Some((id, path)) = l.trim_start().split_once(' ') else {
            continue;
        };
        let name = Path::new(path.trim()).file_name().unwrap_or_default();
        text.push_str(&format!("{id:>4} {}\n", dir.join(name).display()));
    }
    fs::write(dir.join(format!("{}_vinf", f.db)), text)?;
    let dbtxt = dir.join("databases.txt");
    fs::write(
        &dbtxt,
        format!(
            "{}\t{}\tlocalhost\t{}\tfile:{}\n",
            f.db,
            dir.display(),
            dir.display(),
            dir.join("lob").display()
        ),
    )?;
    Ok(Ok(dbtxt))
}
