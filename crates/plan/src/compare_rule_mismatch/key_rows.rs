//! The pieces every compare rule mismatch check shares: the live key values of a class's rows, the string rules
//! of the two engines (10.2's "equal once trailing zero-weight bytes are cut" and develop's VARCHAR compare), and
//! the target names and literals the post-migration fixes SQL prints.
//!
//! Input: the source volume set and line, class objects, attribute names. Output: key tuples per live row, string
//! predicates, SQL text.
//! Source: R14 § 2-3 (workspace#524: 10.2 cuts trailing 0x20, and 0x00 under EN/ISO collations; develop VARCHAR
//! compares with `ti` = false, `ignore_trailing_space` = no); live rows as `lob_scan` reads them (a cleanly shut
//! down source, R2 § 3); develop unique names `lower(owner).class` for a source before user schema (R10 § 3,
//! workspace#511).

use std::cmp::Ordering;
use std::collections::HashMap;
use std::io;

use fastmig_catalog::source_classes::SourceClass;
use fastmig_ondisk::class_object::{ClassObject, SchemaDomain};
use fastmig_ondisk::record::{RecordHeader, RecordLayout, decode_record_attrs, heap_records};
use fastmig_ondisk::value::{Collation, DbType, Value};
use fastmig_ondisk::volume_set::VolumeSet;
use fastmig_ondisk::{Oid, VersionLine};

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

/// Calls `f` with the values of attributes `names` (in that order, `Value::Null` when the row's representation
/// lacks one) for every live row of `c`'s own heap.
pub fn for_each_key(
    vs: &VolumeSet,
    line: VersionLine,
    c: &SourceClass,
    names: &[&[u8]],
    mut f: impl FnMut(&[Value]),
) -> io::Result<()> {
    if c.class.heap.is_null() {
        return Ok(());
    }
    let ids: Vec<Option<i32>> = names
        .iter()
        .map(|n| {
            c.class
                .attributes
                .iter()
                .find(|a| a.name.as_slice() == *n)
                .map(|a| a.id)
        })
        .collect();
    let label = String::from_utf8_lossy(&c.class.name).into_owned();
    let mut key = Vec::with_capacity(names.len());
    for r in heap_records(vs, c.class.heap) {
        let r = r?;
        let h = RecordHeader::read(&r.bytes)
            .map_err(|e| invalid(format!("{label} {:?}: {e}", r.oid)))?;
        if h.is_deleted() {
            continue;
        }
        let layout = RecordLayout::for_repid(line, &c.class, h.repid)
            .map_err(|e| invalid(format!("{label} repid {}: {e}", h.repid)))?;
        let d = decode_record_attrs(line, &layout, &r.bytes, |id| ids.contains(&Some(id)))
            .map_err(|e| invalid(format!("{label} {:?}: {e}", r.oid)))?;
        key.clear();
        for id in &ids {
            let v = layout
                .attrs
                .iter()
                .zip(&d.values)
                .find(|(a, _)| Some(a.id) == *id)
                .map_or(Value::Null, |(_, v)| v.clone());
            key.push(v);
        }
        f(&key);
    }
    Ok(())
}

/// The domain of attribute `name` of `c`.
pub fn attr_domain<'c>(c: &'c ClassObject, name: &[u8]) -> Option<&'c SchemaDomain> {
    c.attributes
        .iter()
        .find(|a| a.name.as_slice() == name)
        .and_then(|a| a.domain.first())
}

/// The develop compare of a VARCHAR column (or a 10.2 VARNCHAR one, which develop reads as VARCHAR and 10.2 compared
/// alike, workspace#554) whose equality 10.2 drew wider, or `None` for a column the rule change does not touch: CHAR
/// and NCHAR compare with trailing spaces ignored on both lines, `binary` (9) keeps spaces on both, an LDML collation
/// is refused by the index pre-check (workspace#491).
pub fn varchar_compare(d: &SchemaDomain) -> Option<Collation> {
    if d.type_id != DbType::Varchar as i32 && d.type_id != DbType::Varnchar as i32 {
        return None;
    }
    Collation::from_id(d.collation_id).filter(|c| *c != Collation::Binary)
}

/// The string of a CHAR/VARCHAR or NCHAR/VARNCHAR value.
pub fn string(v: &Value) -> Option<&[u8]> {
    match v {
        Value::Varchar(s) | Value::Char(s) | Value::Varnchar(s) | Value::Nchar(s) => Some(s),
        _ => None,
    }
}

/// Ends in a byte 10.2 ignores at the end of a compare.
pub fn has_trailing(s: &[u8]) -> bool {
    matches!(s.last(), Some(b' ' | 0))
}

/// `s` without the trailing bytes 10.2 ignores.
pub fn cut_trailing(s: &[u8]) -> &[u8] {
    let end = s
        .iter()
        .rposition(|&b| b != b' ' && b != 0)
        .map_or(0, |i| i + 1);
    &s[..end]
}

/// develop: equal under `c` (VARCHAR, trailing bytes count).
pub fn develop_eq(c: Collation, a: &[u8], b: &[u8]) -> bool {
    c.compare(a, b, false) == Ordering::Equal
}

/// 10.2: equal under `c` once the trailing zero-weight bytes are cut.
pub fn equal_in_10_2(c: Collation, a: &[u8], b: &[u8]) -> bool {
    develop_eq(c, cut_trailing(a), cut_trailing(b))
}

/// The owner name prefixes (already lowercased as the engine does) of a source before user schema, by `db_user` row
/// OID.
pub type OwnerNames = HashMap<Oid, Vec<u8>>;

fn ident(s: &[u8]) -> String {
    format!("[{}]", String::from_utf8_lossy(s).replace(']', "]]"))
}

/// `[owner].[class]` as the target names `c`: its unique name, or `lower(owner).name` for a source before user
/// schema.
pub fn target_class(c: &ClassObject, owners: &OwnerNames) -> String {
    match c.name.iter().position(|&b| b == b'.') {
        Some(p) => format!("{}.{}", ident(&c.name[..p]), ident(&c.name[p + 1..])),
        None => match owners.get(&c.owner) {
            Some(o) => format!("{}.{}", ident(o), ident(&c.name)),
            None => ident(&c.name),
        },
    }
}

pub fn column(name: &[u8]) -> String {
    ident(name)
}

/// A string literal; a NUL byte cannot be written in one and shows as U+FFFD (shortcut: develop partition bounds take
/// plain literals only, no CHR/CONCAT/charset hex (measured workspace#554), so a LIST/RANGE fix whose bound or row
/// holds a NUL is wrong; upgrade when a source with such a partition value shows up).
pub fn literal(s: &[u8]) -> String {
    format!(
        "'{}'",
        String::from_utf8_lossy(s)
            .replace('\0', "\u{FFFD}")
            .replace('\'', "''")
    )
}

/// A value for a warning line.
pub fn show(s: &[u8]) -> String {
    format!("{:?}", String::from_utf8_lossy(s))
}
