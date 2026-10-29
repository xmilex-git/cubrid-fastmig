//! Re-encodes one record of a re-encoded class: decoded under the 11.4 representation it was written with, written
//! in develop's encoding under the class's current representation, with a header free of vacuum residue.
//!
//! Input: the 11.4 class object (current attributes and older representations) and whether the class is MVCC; then
//! one record at a time. Output: the develop record bytes.
//! Source: workspace#482 decision 4 (every record goes to the current repid, repid number kept), R2 § 3 R7.2-R7.3
//! (header after cleanup: 8 B, MVCC flags 0, CHN kept); `RecordLayout::to_develop` (the attribute order the class
//! object writer uses); engine `object_representation_sr.c` `or_get_current_representation` (:2540-2552): an
//! attribute missing from an older representation reads as its original default value (`ORC_ATT_ORIGINAL_VALUE`),
//! NULL when it has none; `mvcc.c` `mvcc_is_mvcc_disabled_class` (root class, `db_serial`, `_db_collation`,
//! `db_ha_apply_info` in 11.4).

use std::collections::HashMap;

use crate::class_object::ClassObject;
use crate::record::layout::RecordLayout;
use crate::record::mvcc_header::RecordHeader;
use crate::record::record_codec::{decode_record, encode_record};
use crate::value::{DecodeError, DecodeResult, OrReader, Value, decode_tagged, value_to_develop};
use crate::{Oid, VersionLine};

/// Turns a record of a heap into its develop form. Implemented by [`ClassReencoder`]; callers with their own row
/// source (system catalog rows) can supply another.
pub trait Reencode: Sync {
    /// The develop record of object `oid` whose current record is `rec` (a whole record, as the home, NEWHOME or
    /// overflow chain holds it).
    fn reencode(&self, oid: Oid, rec: &[u8]) -> DecodeResult<Vec<u8>>;
    /// Whether the class is MVCC (decides the header of a record that goes to overflow pages).
    fn mvcc(&self) -> bool;
}

/// 11.4 class names whose records carry no MVCC header (engine `mvcc_is_mvcc_disabled_class`, plus the root class,
/// which has no class object of its own).
pub fn mvcc_disabled_class(name: &[u8]) -> bool {
    matches!(name, b"db_serial" | b"_db_collation" | b"db_ha_apply_info")
}

pub struct ClassReencoder {
    source: HashMap<u32, RecordLayout>,
    target: RecordLayout,
    /// Original default value of each current attribute (`Value::Null` when it has none), develop form.
    defaults: HashMap<i32, Value>,
    mvcc: bool,
}

impl ClassReencoder {
    /// Prepares the layouts of every representation of `class` (an 11.4 class object).
    pub fn new(class: &ClassObject, mvcc: bool) -> DecodeResult<ClassReencoder> {
        let line = VersionLine::V11_4;
        let current = RecordLayout::for_repid(line, class, class.repid as u32)?;
        let mut source = HashMap::new();
        for r in &class.representations {
            source.insert(
                r.id as u32,
                RecordLayout::for_repid(line, class, r.id as u32)?,
            );
        }
        let mut defaults = HashMap::new();
        for a in &class.attributes {
            let v = if a.original_value.is_empty() {
                Value::Null
            } else {
                let mut r = OrReader::new(&a.original_value);
                value_to_develop(&decode_tagged(line, &mut r, Some(a.original_value.len()))?.1)
            };
            defaults.insert(a.id, v);
        }
        let target = current.to_develop();
        source.insert(current.repid, current);
        Ok(ClassReencoder {
            source,
            target,
            defaults,
            mvcc,
        })
    }

    /// The develop layout records are written under.
    pub fn target(&self) -> &RecordLayout {
        &self.target
    }
}

impl Reencode for ClassReencoder {
    fn reencode(&self, _oid: Oid, rec: &[u8]) -> DecodeResult<Vec<u8>> {
        let h = RecordHeader::read(rec)?;
        let layout = self.source.get(&h.repid).ok_or_else(|| DecodeError {
            at: 0,
            what: format!(
                "record of representation {} the class does not have",
                h.repid
            ),
        })?;
        let d = decode_record(VersionLine::V11_4, layout, rec)?;
        let by_id: HashMap<i32, &Value> =
            layout.attrs.iter().map(|a| a.id).zip(&d.values).collect();
        let values: Vec<Value> = self
            .target
            .attrs
            .iter()
            .map(|a| match by_id.get(&a.id) {
                Some(v) => value_to_develop(v),
                None => self.defaults.get(&a.id).cloned().unwrap_or(Value::Null),
            })
            .collect();
        let header = RecordHeader {
            repid: self.target.repid,
            has_bound_bits: false,
            offset_size: 4,
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
