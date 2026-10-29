//! Reads one engine `unloaddb` output set into the entries the set comparison counts.
//!
//! Input: an unload prefix `<dir>/<name>`; the set is every file `<dir>/<name>_<suffix>` (`_schema`, `_objects`,
//! `_indexes`, `_trigger`, split schema files, ...) except the `*.log` timing logs. Output: per suffix, the entries
//! of that file as byte strings: for `_objects` one entry per logical row, tagged with its `%class` header line (a
//! row continues over a line that ends a string piece `'+` or a set element break `, `); for every other file one
//! entry per line. Bytes, not text: string values keep the database charset (euckr rows are not UTF-8).
//! Source: engine `unload_object.c:process_object` (row = values then `\n`), `process_set` (a `\n` after every
//! 10th element's `, `), `unload_object_file.c:place_text_to_file` (`'+\n '` inside a long string), release/11.4 and
//! develop; set comparison in G3 workspace#483 and ADR 0007.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The files of one unload set: suffix (`objects`, `schema`, ...) → path.
pub fn unload_set(prefix: &Path) -> io::Result<BTreeMap<String, PathBuf>> {
    let dir = match prefix.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let name = prefix.file_name().and_then(|n| n.to_str()).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "unload prefix has no file name",
        )
    })?;
    let head = format!("{name}_");
    let mut set = BTreeMap::new();
    for e in fs::read_dir(&dir)? {
        let e = e?;
        let file = e.file_name();
        let Some(file) = file.to_str() else { continue };
        let Some(suffix) = file.strip_prefix(&head) else {
            continue;
        };
        // unloaddb's own timing log differs on every run
        if suffix.ends_with(".log") || !e.file_type()?.is_file() {
            continue;
        }
        set.insert(suffix.to_string(), e.path());
    }
    Ok(set)
}

/// The entries of one unload file, see the module header.
pub fn entries(suffix: &str, path: &Path) -> io::Result<Vec<Vec<u8>>> {
    let bytes = fs::read(path)?;
    let lines = bytes.split(|&b| b == b'\n').filter(|l| !l.is_empty());
    if suffix != "objects" {
        return Ok(lines.map(<[u8]>::to_vec).collect());
    }
    let mut out = Vec::new();
    let mut class: Vec<u8> = Vec::new();
    let mut row: Option<Vec<u8>> = None;
    for line in lines {
        if let Some(r) = row.as_mut()
            && (r.ends_with(b"'+") || r.ends_with(b", "))
        {
            r.push(b'\n');
            r.extend_from_slice(line);
            continue;
        }
        if let Some(r) = row.take() {
            out.push(r);
        }
        if line.starts_with(b"%class ") {
            class = line.to_vec();
            out.push(line.to_vec());
        } else if line.starts_with(b"%") {
            out.push(line.to_vec());
        } else {
            let mut r = class.clone();
            r.push(b'\t');
            r.extend_from_slice(line);
            row = Some(r);
        }
    }
    if let Some(r) = row {
        out.push(r);
    }
    Ok(out)
}
