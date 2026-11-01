//! The files of one database in its directories, as the engine names them: the log directory's own files, the volume
//! lines of `_vinf`, and the database's line in `databases.txt`; with the removal helpers finalize and `--replace`
//! share.
//!
//! Input: a directory and a database name, a `_vinf` path, a `databases.txt` path. Output: paths; files removed.
//! Source: engine `boot_sr.c:boot_find_rest_permanent_volumes` (`_vinf` lines `%4d %s`, volid ≥ 0 the volumes),
//! `log_manager.c:logpb_delete` (the log directory's files of a database), `databases_file.c:cfg_delete_db`;
//! decision G1 workspace#481 (finalize's delete list), G13 workspace#604 (`--replace`).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Log-directory files of the database (G1): the exact names, plus every `<db>_lgar<digits>` archive and `<db>_lgar_t`.
pub fn log_files(dir: &Path, db: &str) -> io::Result<Vec<PathBuf>> {
    let exact: Vec<String> = ["_lgat", "_lginf", "_vinf", "_dwb"]
        .iter()
        .map(|s| format!("{db}{s}"))
        .collect();
    let archive = format!("{db}_lgar");
    let mut out = Vec::new();
    for e in fs::read_dir(dir)? {
        let name = e?.file_name().to_string_lossy().into_owned();
        if exact.contains(&name)
            || name.strip_prefix(&archive).is_some_and(|r| {
                r == "_t" || (!r.is_empty() && r.bytes().all(|b| b.is_ascii_digit()))
            })
        {
            out.push(dir.join(name));
        }
    }
    out.sort();
    Ok(out)
}

/// The volume lines (volid ≥ 0) of a `_vinf` file, `%4d %s` per line; none when the file is missing or unreadable
/// (the engine then follows the headers).
pub fn vinf_volumes(path: &Path) -> Vec<(i32, PathBuf)> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| {
            let (id, p) = l.trim_start().split_once(' ')?;
            Some((id.parse::<i32>().ok()?, PathBuf::from(p.trim())))
        })
        .filter(|(id, _)| *id >= 0)
        .collect()
}

/// Removes `p`, recorded in `deleted`; a missing file is no error.
pub fn remove_file(p: &Path, deleted: &mut Vec<PathBuf>) -> io::Result<()> {
    match fs::remove_file(p) {
        Ok(()) => {
            deleted.push(p.to_path_buf());
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io::Error::new(e.kind(), format!("{}: {e}", p.display()))),
    }
}

/// Removes the line of `db` from databases.txt (rename over it, so a crash leaves the old or the new file).
pub fn unregister(dbtxt: &Path, db: &str) -> io::Result<()> {
    let text = fs::read_to_string(dbtxt)?;
    let kept: String = text
        .lines()
        .filter(|l| l.split('#').next().unwrap_or("").split_whitespace().next() != Some(db))
        .map(|l| format!("{l}\n"))
        .collect();
    let tmp = dbtxt.with_extension("fastmig-unregister");
    fs::write(&tmp, kept)?;
    fs::File::open(&tmp)?.sync_all()?;
    fs::rename(&tmp, dbtxt)
}
