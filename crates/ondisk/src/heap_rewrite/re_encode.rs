//! Re-encodes one record of a re-encoded class: decoded under the 10.2/11.4 representation it was written with, written
//! in develop's encoding under the class's current representation, with a header free of vacuum residue.
//!
//! Input: the 10.2/11.4 class object (current attributes and older representations) and whether the class is MVCC; then
//! one record at a time. Output: the develop record bytes.
//! Source: workspace#482 decision 4 (every record goes to the current repid, repid number kept), R2 § 3 R7.2-R7.3
//! (header after cleanup: 8 B, MVCC flags 0, CHN kept); `RecordLayout::to_develop` (the attribute order the class
//! object writer uses); engine `object_representation_sr.c` `or_get_current_representation` (:2540-2552): an
//! attribute missing from an older representation reads as its original default value (`ORC_ATT_ORIGINAL_VALUE`),
//! NULL when it has none; `mvcc.c` `mvcc_is_mvcc_disabled_class` (root class, `db_serial`, `_db_collation`,
//! `db_ha_apply_info` in 11.4).

use std::collections::HashMap;
use std::path::PathBuf;

use crate::class_object::ClassObject;
use crate::record::layout::RecordLayout;
use crate::record::mvcc_header::RecordHeader;
use crate::record::record_codec::{decode_record, encode_record};
use crate::value::{
    DecodeError, DecodeResult, OrReader, Value, decode_tagged, develop_lob_dir,
    develop_lob_rel_path, value_to_develop,
};
use crate::{Hfid, Oid, VersionLine};

/// Turns a record of a heap into its develop form. Implemented by [`ClassReEncoder`]; callers with their own row
/// source (system catalog rows) can supply another.
pub trait ReEncode: Sync {
    /// The develop record of object `oid` whose current record is `rec` (a whole record, as the home, NEWHOME or
    /// overflow chain holds it).
    fn re_encode(&self, oid: Oid, rec: &[u8]) -> DecodeResult<Vec<u8>>;
    /// Whether the class is MVCC (decides the header of a record that goes to overflow pages).
    fn mvcc(&self) -> bool;
}

/// 11.4 class names whose records carry no MVCC header (engine `mvcc_is_mvcc_disabled_class`, plus the root class,
/// which has no class object of its own).
pub fn mvcc_disabled_class(name: &[u8]) -> bool {
    matches!(name, b"db_serial" | b"_db_collation" | b"db_ha_apply_info")
}

/// One representation records may be written under: its layout and, per attribute of the target layout, that
/// attribute's position in it (`None`: the attribute is newer than the representation). Resolved once per class so
/// the row loop does no lookup by attribute id (BR-04, workspace#536).
struct SourceRepr {
    layout: RecordLayout,
    positions: Vec<Option<usize>>,
}

pub struct ClassReEncoder {
    /// The source line records are decoded with.
    line: VersionLine,
    /// The current representation first; a class has a handful.
    source: Vec<SourceRepr>,
    target: RecordLayout,
    /// Original default value of each target attribute, in target order (`Value::Null` when it has none), develop
    /// form.
    defaults: Vec<Value>,
    mvcc: bool,
    /// The class heap and the source lob-base-path, when LOB locators move to develop's per-attribute directories.
    lob_dirs: Option<(Hfid, Option<PathBuf>)>,
}

impl ClassReEncoder {
    /// Prepares the layouts of every representation of `class`, a class object of source line `line` (10.2 or 11.4).
    pub fn new(class: &ClassObject, line: VersionLine, mvcc: bool) -> DecodeResult<ClassReEncoder> {
        let current = RecordLayout::for_repid(line, class, class.repid as u32)?;
        let target = current.to_develop();
        let repr = |layout: RecordLayout| SourceRepr {
            positions: target
                .attrs
                .iter()
                .map(|t| layout.attrs.iter().position(|a| a.id == t.id))
                .collect(),
            layout,
        };
        let mut source = vec![repr(current)];
        for r in &class.representations {
            if r.id as u32 != source[0].layout.repid {
                source.push(repr(RecordLayout::for_repid(line, class, r.id as u32)?));
            }
        }
        let mut defaults = HashMap::new();
        for a in &class.attributes {
            let v = if a.original_value.is_empty() {
                Value::Null
            } else {
                let mut r = OrReader::new(&a.original_value);
                value_to_develop(decode_tagged(line, &mut r, Some(a.original_value.len()))?.1)
            };
            defaults.insert(a.id, v);
        }
        let defaults = target
            .attrs
            .iter()
            .map(|a| defaults.remove(&a.id).unwrap_or(Value::Null))
            .collect();
        Ok(ClassReEncoder {
            line,
            source,
            target,
            defaults,
            mvcc,
            lob_dirs: None,
        })
    }

    /// Rewrites each LOB locator to its file's place in develop's directory of (`heap`, attribute): a relative
    /// locator or an absolute one under `lob_base`; an absolute locator outside it stays (G6 workspace#488).
    /// The copy step puts the files there (workspace#507, CBRD-23700).
    pub fn with_lob_dirs(mut self, heap: Hfid, lob_base: Option<PathBuf>) -> Self {
        self.lob_dirs = Some((heap, lob_base));
        self
    }

    fn relocate_lob(&self, attrid: i32, v: Value) -> Value {
        match (v, &self.lob_dirs) {
            (Value::Elo(mut e), Some((heap, base))) => {
                let dir = develop_lob_dir(*heap, attrid);
                if let Some(rel) = develop_lob_rel_path(&e.lob_path(), base.as_deref(), &dir) {
                    e.locator = format!("file:{}", rel.display()).into_bytes();
                }
                Value::Elo(e)
            }
            (v, _) => v,
        }
    }

    /// The develop layout records are written under.
    pub fn target(&self) -> &RecordLayout {
        &self.target
    }
}

impl ReEncode for ClassReEncoder {
    fn re_encode(&self, _oid: Oid, rec: &[u8]) -> DecodeResult<Vec<u8>> {
        let h = RecordHeader::read(rec)?;
        let src = self
            .source
            .iter()
            .find(|s| s.layout.repid == h.repid)
            .ok_or_else(|| DecodeError {
                at: 0,
                what: format!(
                    "record of representation {} the class does not have",
                    h.repid
                ),
            })?;
        let mut d = decode_record(self.line, &src.layout, rec)?;
        let values: Vec<Value> = self
            .target
            .attrs
            .iter()
            .zip(&src.positions)
            .zip(&self.defaults)
            .map(
                |((a, p), default)| match p.and_then(|j| d.values.get_mut(j)) {
                    // moved, not copied: the decoded record is dropped after this
                    Some(v) => {
                        self.relocate_lob(a.id, value_to_develop(std::mem::replace(v, Value::Null)))
                    }
                    None => default.clone(),
                },
            )
            .collect();
        let header = RecordHeader {
            repid: self.target.repid,
            has_bound_bits: false,
            offset_size: 1, // the smallest the engine rule allows, as heap_attrinfo_transform_to_disk
            mvcc_flags: 0,
            chn: h.chn,
            insid: None,
            delid: None,
            prev_version_lsa: None,
        };
        encode_record(
            VersionLine::Develop,
            &self.target,
            &header,
            &values,
            self.mvcc,
        )
    }

    fn mvcc(&self) -> bool {
        self.mvcc
    }
}
