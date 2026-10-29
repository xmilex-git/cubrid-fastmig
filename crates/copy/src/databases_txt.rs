//! The `databases.txt` directory file: finds the source database's paths and registers the target database.
//!
//! Input: the file path and a database name, or a new entry. Output: the entry's paths, or the file with one line
//! appended. A target name already present is refused.
//! Line format (whitespace separated): `db-name vol-path db-host log-path lob-base-path`; lines starting with `#`
//! are comments. Written the way the engine writes it.
//! Source: engine `src/base/databases_file.c:cfg_write_directory`, `cfg_read_directory` (develop, same in
//! release/11.4); decision G1 workspace#481 (register in the target's `databases.txt` only after success).

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DbEntry {
    pub name: String,
    pub vol_path: PathBuf,
    pub host: String,
    pub log_path: PathBuf,
    /// As written, e.g. `file:/path/lob`; empty when the line has none.
    pub lob_base_path: String,
}

/// Every entry of the file; a missing file has none.
pub fn read(file: &Path) -> io::Result<Vec<DbEntry>> {
    let text = match fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    for line in text.lines() {
        let t: Vec<&str> = line.split_whitespace().collect();
        if t.is_empty() || t[0].starts_with('#') {
            continue;
        }
        if t.len() < 4 {
            return Err(io::Error::other(format!(
                "{}: malformed line: {line}",
                file.display()
            )));
        }
        out.push(DbEntry {
            name: t[0].to_string(),
            vol_path: PathBuf::from(t[1]),
            host: t[2].to_string(),
            log_path: PathBuf::from(t[3]),
            lob_base_path: t.get(4).unwrap_or(&"").to_string(),
        });
    }
    Ok(out)
}

/// The entry named `name`.
pub fn find(file: &Path, name: &str) -> io::Result<DbEntry> {
    read(file)?
        .into_iter()
        .find(|e| e.name == name)
        .ok_or_else(|| io::Error::other(format!("{name} is not in {}", file.display())))
}

/// Fails when `name` is already registered in `file` (checked before the run and again by [`register`]).
pub fn check_name_free(file: &Path, name: &str) -> io::Result<()> {
    if read(file)?.iter().any(|e| e.name == name) {
        return Err(io::Error::other(format!(
            "{name} is already registered in {}",
            file.display()
        )));
    }
    Ok(())
}

/// Appends `entry` to `file` (creating it with the engine's header line when missing).
pub fn register(file: &Path, entry: &DbEntry) -> io::Result<()> {
    check_name_free(file, &entry.name)?;
    let fresh = !file.exists();
    let mut f = OpenOptions::new().append(true).create(true).open(file)?;
    let mut line = String::new();
    if fresh {
        line.push_str("#db-name\tvol-path\t\tdb-host\t\tlog-path\t\tlob-base-path\n");
    }
    let pad = if entry.name.len() < 8 { "\t" } else { "" };
    line.push_str(&format!(
        "{}{pad}\t{}\t{}\t{} \t{} \n",
        entry.name,
        entry.vol_path.display(),
        entry.host,
        entry.log_path.display(),
        entry.lob_base_path
    ));
    f.write_all(line.as_bytes())?;
    f.sync_data()
}
