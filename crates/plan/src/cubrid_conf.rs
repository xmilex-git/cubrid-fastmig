//! Reads `ignore_trailing_space` from a `cubrid.conf`: the one server parameter that changes how VARCHAR keys
//! order and is recorded nowhere in the database.
//!
//! Input: a `cubrid.conf` path and the database name. Output: whether the server of that database runs with
//! `ignore_trailing_space=yes`.
//! Source: engine `system_parameter.c` (`PRM_ID_IGNORE_TRAILING_SPACE`, default no, from 11.0; sections `[common]`
//! and `[@<db>]` apply to a server, a later line wins, names are case-insensitive, booleans yes/no, true/false,
//! on/off, 1/0); R14 § 8 (workspace#524); decision G10 workspace#541 (target `yes` refuses: the tool orders VARCHAR
//! keys with trailing spaces counted; source `yes` turns the foreign key and LIST/RANGE checks on).

use std::fs;
use std::io;
use std::path::Path;

pub const PARAMETER: &str = "ignore_trailing_space";

pub fn ignore_trailing_space(conf: &Path, db: &str) -> io::Result<bool> {
    let text = fs::read_to_string(conf)
        .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", conf.display())))?;
    let mut applies = false;
    let mut value = false;
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
        if applies && k.trim().eq_ignore_ascii_case(PARAMETER) {
            value = matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "yes" | "true" | "on" | "1"
            );
        }
    }
    Ok(value)
}
