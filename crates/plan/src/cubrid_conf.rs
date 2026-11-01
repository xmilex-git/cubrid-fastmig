//! Reads server parameters from a `cubrid.conf` the way the server of one database sees them: `ignore_trailing_space`
//! (the one parameter that changes how VARCHAR keys order and is recorded nowhere in the database) and the target
//! defaults of migrate (`db_volume_size`, `data_buffer_size`, `temp_file_max_size_in_pages`, workspace#604).
//!
//! Input: a `cubrid.conf` path, the database name and the parameter name. Output: the value the server of that
//! database runs with, or none when the file does not set it.
//! Source: engine `system_parameter.c` (`PRM_ID_IGNORE_TRAILING_SPACE`, default no, from 11.0; sections `[common]`
//! and `[@<db>]` apply to a server, a later line wins, names are case-insensitive, booleans yes/no, true/false,
//! on/off, 1/0); R14 § 8 (workspace#524); decision G10 workspace#541 (target `yes` refuses: the tool orders VARCHAR
//! keys with trailing spaces counted; source `yes` turns the foreign key and LIST/RANGE checks on); G13
//! workspace#604 (target conf defaults).

use std::fs;
use std::io;
use std::path::Path;

pub const PARAMETER: &str = "ignore_trailing_space";

/// The value of `name` for the server of `db`: the last line of `[common]` or `[@<db>]` that sets it.
pub fn parameter(conf: &Path, db: &str, name: &str) -> io::Result<Option<String>> {
    let text = fs::read_to_string(conf)
        .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", conf.display())))?;
    let mut applies = false;
    let mut value = None;
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if let Some(section) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            let s = section.trim();
            applies = s.eq_ignore_ascii_case("common")
                || s.strip_prefix('@')
                    .is_some_and(|d| d.eq_ignore_ascii_case(db));
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        if applies && k.trim().eq_ignore_ascii_case(name) {
            value = Some(v.trim().to_string());
        }
    }
    Ok(value)
}

pub fn ignore_trailing_space(conf: &Path, db: &str) -> io::Result<bool> {
    Ok(parameter(conf, db, PARAMETER)?
        .is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "yes" | "true" | "on" | "1")))
}
