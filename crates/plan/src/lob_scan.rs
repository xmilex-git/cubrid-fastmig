//! The LOB locator scan: reads every live record of the classes with a BLOB/CLOB attribute and sorts their
//! locators into referenced LOB files (the LOB manifest), locators the tool leaves alone, and the orphan LOB files
//! of the lob-base-path.
//!
//! Input: the source volume set, its line, its class objects and its lob-base-path directory. Output: [`LobScan`].
//! Source: decision G6 workspace#488 2-4 (one scan per migration, referenced files only; an absolute locator under
//! the base is a referenced file to rewrite as relative, one outside the base is left as is; orphans reported);
//! ADR 0009; P1b record codec (`RecordLayout::for_repid`, `decode_record`, `Elo::lob_path`).

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use fastmig_catalog::source_classes::SourceClass;
use fastmig_ondisk::VersionLine;
use fastmig_ondisk::class_object::{ClassType, SchemaDomain};
use fastmig_ondisk::record::{RecordHeader, RecordLayout, decode_record, heap_records};
use fastmig_ondisk::value::{DbType, LobPath, Value};
use fastmig_ondisk::volume_set::VolumeSet;

/// One referenced LOB file, a line of the LOB manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReferencedLobFile {
    pub size: u64,
    /// Relative to the lob-base-path, e.g. `ces_014/<meta>.<20 digits>_<4 digits>`.
    pub rel_path: PathBuf,
}

#[derive(Debug, Default)]
pub struct LobScan {
    /// The LOB manifest: one entry per distinct file, sorted by path.
    pub referenced: Vec<ReferencedLobFile>,
    /// Locators stored as `file:/<base>/...`: the record must be rewritten to the relative form (G6 3).
    pub absolute_under_base: u64,
    /// `file:/...` outside the base: neither copied nor deleted; the target opens the same path.
    pub outside_base: Vec<ReferencedLobFile>,
    /// Locators of another scheme (`owfs:`, `local:`).
    pub unsupported: Vec<String>,
    /// Files under the base no locator names: neither copied nor deleted, only reported.
    pub orphans: Vec<ReferencedLobFile>,
}

impl LobScan {
    pub fn referenced_bytes(&self) -> u64 {
        self.referenced.iter().map(|f| f.size).sum()
    }

    pub fn orphan_bytes(&self) -> u64 {
        self.orphans.iter().map(|f| f.size).sum()
    }
}

fn has_lob(d: &[SchemaDomain]) -> bool {
    d.iter().any(|d| {
        d.type_id == DbType::Blob as i32
            || d.type_id == DbType::Clob as i32
            || has_lob(&d.set_domain)
    })
}

fn lob_class(c: &SourceClass) -> bool {
    let k = &c.class;
    k.class_type != ClassType::Vclass
        && !k.heap.is_null()
        && (k.attributes.iter().any(|a| has_lob(&a.domain))
            || k.representations
                .iter()
                .flat_map(|r| &r.attributes)
                .any(|a| has_lob(&a.domain)))
}

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

fn walk(v: &Value, base: Option<&Path>, out: &mut LobScan, files: &mut BTreeMap<PathBuf, u64>) {
    match v {
        Value::Set(s) => s
            .elements
            .iter()
            .for_each(|e| walk(&e.value, base, out, files)),
        Value::Elo(e) => {
            let size = e.size.max(0) as u64;
            match e.lob_path() {
                LobPath::Relative(p) => {
                    files.insert(PathBuf::from(String::from_utf8_lossy(p).into_owned()), size);
                }
                LobPath::Absolute(p) => {
                    let p = PathBuf::from(String::from_utf8_lossy(p).into_owned());
                    match base.and_then(|b| p.strip_prefix(b).ok()) {
                        Some(rel) => {
                            out.absolute_under_base += 1;
                            files.insert(rel.to_path_buf(), size);
                        }
                        None => out
                            .outside_base
                            .push(ReferencedLobFile { size, rel_path: p }),
                    }
                }
                LobPath::Unsupported(l) => out
                    .unsupported
                    .push(String::from_utf8_lossy(l).into_owned()),
            }
        }
        _ => {}
    }
}

/// Every file below `dir`, relative to `base`.
fn list_files(base: &Path, dir: &Path, out: &mut Vec<ReferencedLobFile>) -> io::Result<()> {
    for e in fs::read_dir(dir)? {
        let e = e?;
        let m = e.metadata()?;
        if m.is_dir() {
            list_files(base, &e.path(), out)?;
        } else {
            let p = e.path();
            out.push(ReferencedLobFile {
                size: m.len(),
                rel_path: p.strip_prefix(base).unwrap_or(&p).to_path_buf(),
            });
        }
    }
    Ok(())
}

pub fn scan(
    vs: &VolumeSet,
    line: VersionLine,
    classes: &[SourceClass],
    lob_base: Option<&Path>,
) -> io::Result<LobScan> {
    let mut out = LobScan::default();
    let mut files = BTreeMap::new();
    for c in classes.iter().filter(|c| lob_class(c)) {
        let name = String::from_utf8_lossy(&c.class.name);
        for r in heap_records(vs, c.class.heap) {
            let r = r?;
            let h = RecordHeader::read(&r.bytes)
                .map_err(|e| invalid(format!("{name} {:?}: {e}", r.oid)))?;
            if h.is_deleted() {
                continue;
            }
            let layout = RecordLayout::for_repid(line, &c.class, h.repid)
                .map_err(|e| invalid(format!("{name} repid {}: {e}", h.repid)))?;
            let d = decode_record(line, &layout, &r.bytes)
                .map_err(|e| invalid(format!("{name} {:?}: {e}", r.oid)))?;
            d.values
                .iter()
                .for_each(|v| walk(v, lob_base, &mut out, &mut files));
        }
    }
    out.referenced = files
        .into_iter()
        .map(|(rel_path, size)| ReferencedLobFile { size, rel_path })
        .collect();
    if let Some(base) = lob_base.filter(|b| b.is_dir()) {
        let mut all = Vec::new();
        list_files(base, base, &mut all)?;
        out.orphans = all
            .into_iter()
            .filter(|f| {
                out.referenced
                    .binary_search_by(|r| r.rel_path.cmp(&f.rel_path))
                    .is_err()
            })
            .collect();
    }
    Ok(out)
}
