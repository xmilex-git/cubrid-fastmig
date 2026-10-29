//! The LOB manifest `<dest>/fastmig/lob-manifest`: the referenced LOB files of the source, one per line as
//! `<size>\t<path relative to the source lob-base-path>`, plus `\t<path relative to the target lob-base-path>` when the
//! file moves (11.4 → develop, workspace#507). Built once per migration (by the scan that decodes LOB locators,
//! `fastmig_plan::lob_scan`) and shared by the copy step, the verifier and finalize.
//!
//! Input: entries, or the file. Output: the file, or the entries.
//! Source: decision G6 workspace#488; ADR 0009.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub use fastmig_plan::ReferencedLobFile;

/// A manifest path field: relative, without tab or newline.
fn text_of(p: &Path) -> io::Result<&str> {
    p.to_str()
        .filter(|s| !s.contains(['\n', '\t']) && p.is_relative())
        .ok_or_else(|| io::Error::other(format!("bad LOB path {}", p.display())))
}

pub fn write(path: &Path, entries: &[ReferencedLobFile]) -> io::Result<()> {
    let mut text = String::new();
    for e in entries {
        let rel = text_of(&e.rel_path)?;
        if e.target_rel == e.rel_path {
            text.push_str(&format!("{}\t{rel}\n", e.size));
        } else {
            text.push_str(&format!("{}\t{rel}\t{}\n", e.size, text_of(&e.target_rel)?));
        }
    }
    fs::write(path, text)
}

pub fn read(path: &Path) -> io::Result<Vec<ReferencedLobFile>> {
    let mut out = Vec::new();
    for line in fs::read_to_string(path)?.lines() {
        let (size, rel) = line.split_once('\t').ok_or_else(|| {
            io::Error::other(format!("{}: malformed line: {line}", path.display()))
        })?;
        let size = size
            .parse()
            .map_err(|_| io::Error::other(format!("{}: bad size: {line}", path.display())))?;
        let (rel, target) = rel.split_once('\t').unwrap_or((rel, rel));
        out.push(ReferencedLobFile {
            size,
            rel_path: PathBuf::from(rel),
            target_rel: PathBuf::from(target),
        });
    }
    Ok(out)
}
