//! The volume information file `<db>_vinf` in the log directory: one line per database file, `%4d %s`.
//!
//! Input: the database name, the log directory and the target volume paths in volid order. Output: the file, in
//! createdb's order: -5 the file itself, -4 `_lginf`, -3 `_bkvinf`, -2 `_lgat`, then volumes 0.. .
//! The engine rebuilds this file from the volume header chain when it is missing or its count is wrong; writing it
//! keeps the first boot from following stale paths.
//! Source: engine `src/transaction/log_page_buffer.c:logpb_create_volume_info`, `logpb_add_volume` (develop,
//! release/11.4); R5 § 2.8.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Engine `fileio_make_volume_info_name` and siblings: `<log_dir>/<db><suffix>`.
pub fn log_dir_file(log_dir: &Path, db: &str, suffix: &str) -> PathBuf {
    log_dir.join(format!("{db}{suffix}"))
}

/// Writes `<log_dir>/<db>_vinf` (new file).
pub fn write(log_dir: &Path, db: &str, volumes: &[PathBuf]) -> io::Result<PathBuf> {
    let path = log_dir_file(log_dir, db, "_vinf");
    let mut text = String::new();
    for (volid, suffix) in [
        (-5, "_vinf"),
        (-4, "_lginf"),
        (-3, "_bkvinf"),
        (-2, "_lgat"),
    ] {
        text.push_str(&format!(
            "{volid:4} {}\n",
            log_dir_file(log_dir, db, suffix).display()
        ));
    }
    for (volid, v) in volumes.iter().enumerate() {
        text.push_str(&format!("{volid:4} {}\n", v.display()));
    }
    fs::write(&path, text)?;
    Ok(path)
}
