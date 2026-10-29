//! Registers the target database in the target engine's `databases.txt` by appending one line, written the way the
//! engine writes it; a name already present is refused. Reading entries is [`fastmig_ondisk::databases_txt`]'s.
//!
//! Input: the file path and the new entry. Output: the file with the line appended (created with the engine's
//! header line when missing).
//! Source: engine `src/base/databases_file.c:cfg_write_directory` (develop, same in release/11.4); decision G1
//! workspace#481 (register only after success, refuse a duplicate name).

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

use fastmig_ondisk::databases_txt::{DatabaseEntry, find};

/// Fails when `name` is already registered in `file` (checked before the run and again by [`register`]).
pub fn check_name_free(file: &Path, name: &str) -> io::Result<()> {
    let text = match fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if find(&text, name).is_some() {
        return Err(io::Error::other(format!(
            "{name} is already registered in {}",
            file.display()
        )));
    }
    Ok(())
}

/// Appends `entry` to `file`.
pub fn register(file: &Path, entry: &DatabaseEntry) -> io::Result<()> {
    check_name_free(file, &entry.name)?;
    let fresh = !file.exists();
    let mut f = OpenOptions::new().append(true).create(true).open(file)?;
    let mut line = String::new();
    if fresh {
        line.push_str("#db-name\tvol-path\t\tdb-host\t\tlog-path\t\tlob-base-path\n");
    }
    let pad = if entry.name.len() < 8 { "\t" } else { "" };
    line.push_str(&format!(
        "{}{pad}\t{}\t{}\t{} ",
        entry.name,
        entry.vol_path.display(),
        entry.hosts,
        entry.log_path.display()
    ));
    if let Some(lob) = &entry.lob_base_path {
        line.push_str(&format!("\t{lob} "));
    }
    line.push('\n');
    f.write_all(line.as_bytes())?;
    f.sync_data()
}
