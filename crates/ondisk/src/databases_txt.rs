//! `databases.txt`: the directory of registered databases; finds one database's volume and log directories.
//!
//! Input: the text of a `databases.txt` and a database name. Output: that entry.
//! Source: engine `src/base/databases_file.c` (`cfg_read_directory`: whitespace-separated
//! `db-name vol-path db-host log-path [lob-base-path]`, `#` comments), same in release/11.4 and develop (R1 § 2.10);
//! the source is named by DB name + databases.txt (G1, workspace#481).

use std::io;
use std::path::{Path, PathBuf};

/// One registered database.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DatabaseEntry {
    pub name: String,
    /// Directory of the first volume, which is `<vol_path>/<name>`.
    pub vol_path: PathBuf,
    pub hosts: String,
    pub log_path: PathBuf,
    /// As written (`file:<dir>` for a local LOB store), absent in old entries.
    pub lob_base_path: Option<String>,
}

impl DatabaseEntry {
    /// The first volume (volid 0) of the database.
    pub fn first_volume(&self) -> PathBuf {
        self.vol_path.join(&self.name)
    }
}

/// Finds `name` in the `databases.txt` text.
pub fn find(text: &str, name: &str) -> Option<DatabaseEntry> {
    text.lines()
        .map(|l| l.split('#').next().unwrap_or(""))
        .map(|l| l.split_whitespace().collect::<Vec<_>>())
        .find(|f| f.len() >= 4 && f[0] == name)
        .map(|f| DatabaseEntry {
            name: f[0].to_string(),
            vol_path: PathBuf::from(f[1]),
            hosts: f[2].to_string(),
            log_path: PathBuf::from(f[3]),
            lob_base_path: f.get(4).map(|s| s.to_string()),
        })
}

/// Reads `path` and finds `name`; NotFound if the database is not registered.
pub fn lookup(path: &Path, name: &str) -> io::Result<DatabaseEntry> {
    let text = std::fs::read_to_string(path)?;
    find(&text, name).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("{name} not in {}", path.display()),
        )
    })
}
