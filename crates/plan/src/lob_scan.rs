//! The LOB locator scan: reads every live record of the classes with a BLOB/CLOB attribute and sorts their
//! locators into referenced LOB files (the LOB manifest), locators the tool leaves alone, and the orphan LOB files
//! of the lob-base-path.
//!
//! Input: the source volume set, its line, its class objects and its lob-base-path directory. Output: [`LobScan`].
//! Source: decision G6 workspace#488 2-4 (one scan per migration, referenced files only; an absolute locator under
//! the base is a referenced file to rewrite as relative, one outside the base is left as is; orphans reported);
//! ADR 0009; P1b record codec (`RecordLayout::for_repid`, `decode_record`, `Elo::lob_path`); workspace#507 (11.4 →
//! develop: each referenced file goes to develop's directory of its heap and attribute, `develop_lob_rel_path`;
//! every LOB attribute of a heap gets that directory, rows or not, as the engine's CREATE TABLE makes it).

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use fastmig_catalog::lob_dir_heap::lob_dir_heap;
use fastmig_catalog::source_classes::SourceClass;
use fastmig_ondisk::VersionLine;
use fastmig_ondisk::class_object::{ClassType, SchemaDomain};
use fastmig_ondisk::record::{RecordHeader, RecordLayout, decode_record, heap_records};
use fastmig_ondisk::value::{DbType, LobPath, Value, develop_lob_dir, develop_lob_rel_path};
use fastmig_ondisk::volume_set::VolumeSet;

/// One referenced LOB file, a line of the LOB manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReferencedLobFile {
    pub size: u64,
    /// Relative to the lob-base-path, e.g. `ces_014/<meta>.<20 digits>_<4 digits>`.
    pub rel_path: PathBuf,
    /// Relative to the target's lob-base-path: `rel_path`, or on 11.4 → develop `<develop_lob_dir>/<rel_path>`.
    pub target_rel: PathBuf,
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
    /// 11.4 → develop: the develop LOB directory of every LOB attribute of every heap, relative to the target base.
    pub lob_dirs: Vec<PathBuf>,
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

/// A referenced file's size and target path, keyed by its source path.
type Files = BTreeMap<PathBuf, (u64, PathBuf)>;

/// `dir`: the develop LOB directory of the attribute holding `v`, on 11.4 → develop.
fn walk(v: &Value, base: Option<&Path>, dir: Option<&str>, out: &mut LobScan, files: &mut Files) {
    match v {
        // a collection element is not an attribute value of its own: no develop directory
        Value::Set(s) => s
            .elements
            .iter()
            .for_each(|e| walk(&e.value, base, None, out, files)),
        Value::Elo(e) => {
            let size = e.size.max(0) as u64;
            let lob = e.lob_path();
            let rel = match lob {
                LobPath::Relative(p) => PathBuf::from(String::from_utf8_lossy(p).into_owned()),
                LobPath::Absolute(p) => {
                    let p = PathBuf::from(String::from_utf8_lossy(p).into_owned());
                    match base.and_then(|b| p.strip_prefix(b).ok()) {
                        Some(rel) => {
                            out.absolute_under_base += 1;
                            rel.to_path_buf()
                        }
                        None => {
                            out.outside_base.push(ReferencedLobFile {
                                size,
                                target_rel: p.clone(),
                                rel_path: p,
                            });
                            return;
                        }
                    }
                }
                LobPath::Unsupported(l) => {
                    out.unsupported
                        .push(String::from_utf8_lossy(l).into_owned());
                    return;
                }
            };
            let target = dir
                .and_then(|d| develop_lob_rel_path(&lob, base, d))
                .unwrap_or_else(|| rel.clone());
            files.insert(rel, (size, target));
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
            let rel = p.strip_prefix(base).unwrap_or(&p).to_path_buf();
            out.push(ReferencedLobFile {
                size: m.len(),
                target_rel: rel.clone(),
                rel_path: rel,
            });
        }
    }
    Ok(())
}

/// `develop_dirs`: the pair is 11.4 → develop, so referenced files and their locators move to develop's
/// per-attribute directories (workspace#507).
pub fn scan(
    vs: &VolumeSet,
    line: VersionLine,
    classes: &[SourceClass],
    lob_base: Option<&Path>,
    develop_dirs: bool,
) -> io::Result<LobScan> {
    let mut out = LobScan::default();
    let mut files = Files::new();
    for c in classes.iter().filter(|c| lob_class(c)) {
        let name = String::from_utf8_lossy(&c.class.name);
        let heap = c.class.heap;
        let files_heap = lob_dir_heap(c, classes);
        if develop_dirs {
            // the engine makes these at CREATE TABLE / ADD COLUMN, rows or not (develop `xlob_create_dir`)
            out.lob_dirs.extend(
                c.class
                    .attributes
                    .iter()
                    .filter(|a| has_lob(&a.domain))
                    .map(|a| PathBuf::from(develop_lob_dir(heap, a.id))),
            );
        }
        for r in heap_records(vs, heap) {
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
            for (a, v) in layout.attrs.iter().zip(&d.values) {
                let dir = develop_dirs.then(|| develop_lob_dir(files_heap, a.id));
                walk(v, lob_base, dir.as_deref(), &mut out, &mut files);
            }
        }
    }
    out.referenced = files
        .into_iter()
        .map(|(rel_path, (size, target_rel))| ReferencedLobFile {
            size,
            rel_path,
            target_rel,
        })
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
