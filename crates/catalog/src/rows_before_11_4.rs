//! Brings the same-slot rows of an 11.2 or 11.3 source to the 11.4 shape the rest of regeneration reads: grants gain
//! their object type, the serial's `att_name` becomes `attr_name`.
//!
//! Input: the source version, a same-slot table's source name and one of its rows. Output: the row as an 11.4
//! source would hold it, or the unchanged row for an 11.4 source.
//! Source: R8 § 2.11 (`db_authorization.grants` {class, grantor, cache} → {type, class, grantor, cache}, the old
//! entries are classes, `DB_OBJECT_CLASS` 0, dbtype_def.h:492@11.4, 1ea4a86e0 #5438; `db_serial.att_name` →
//! `attr_name`, b85c07b27 #5346). R12 workspace#521 confirms these against the engine's own upgrade path; the
//! stored procedure tables wait for it (plan refuses an 11.2/11.3 source until then, workspace#529).

use fastmig_ondisk::value::{DbType, DecodeError, DecodeResult, Domain, SetElement, Value};
use fastmig_ondisk::{SourceVersion, VersionLine};

use crate::class_rows::Row;

/// `DB_OBJECT_CLASS`.
const OBJECT_CLASS: i32 = 0;
/// Elements of one grant entry before 11.4 (`GRANT_ENTRY_LENGTH` 3, authenticate.c:131@release/11.3).
const OLD_GRANT_ENTRY_LENGTH: usize = 3;

pub fn to_11_4_shape(version: SourceVersion, src_name: &str, mut row: Row) -> DecodeResult<Row> {
    if !matches!(version, SourceVersion::V11_2 | SourceVersion::V11_3) {
        return Ok(row);
    }
    match src_name {
        "db_serial" => {
            if let Some(v) = row.values.remove("att_name") {
                row.values.insert("attr_name".into(), v);
            }
        }
        "db_authorization" => {
            if let Some(Value::Set(s)) = row.values.get_mut("grants") {
                if !s.elements.len().is_multiple_of(OLD_GRANT_ENTRY_LENGTH) {
                    return Err(DecodeError {
                        at: 0,
                        what: format!("db_authorization.grants: {} elements", s.elements.len()),
                    });
                }
                let tag = Some(Domain::builtin(VersionLine::V11_4, DbType::Integer));
                s.elements = std::mem::take(&mut s.elements)
                    .chunks(OLD_GRANT_ENTRY_LENGTH)
                    .flat_map(|entry| {
                        let ty = SetElement {
                            tag: tag.clone(),
                            value: Value::Integer(OBJECT_CLASS),
                        };
                        std::iter::once(ty).chain(entry.iter().cloned())
                    })
                    .collect();
            }
        }
        _ => {}
    }
    Ok(row)
}
