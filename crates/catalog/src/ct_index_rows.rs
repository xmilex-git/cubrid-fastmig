//! Derives a class's `_db_index` and `_db_index_key` rows from the constraints in its property sequence (develop or 11.4).
//!
//! Input: the class's `_db_class` row (the rows' parent), the class object of the sink's line, the migrate start time.
//! Output: the rows, pushed into the caller's sink; the value of the `_db_class.indexes` column.
//! Source: engine develop @682b080b2 `catalog_class.c`: `catcls_get_property_set` (families in the order primary
//! key, unique, reverse unique, index, reverse index, foreign key; constraints in stored order inside a family),
//! `catcls_get_or_value_from_indexes` (FK info: raw referenced class OID, delete/update rule, match option; the
//! optional element of other indexes: a list of prefix lengths, or named `*PLID*` prefix / `*FP*` filter / `*FI*`
//! function entries), `catcls_convert_attr_id_to_name` (key attribute ids → names of the instance attributes),
//! `catcls_set_or_value_timestamps`; R4 § 7 R5-R6. Filter and function indexes never reach here: they leave the
//! target catalog (workspace#486), and the template's system classes have none. An 11.4 target: release/11.4
//! `catcls_get_or_value_from_indexes` (the columns develop added left out by the sink; a DEDUPLICATE index's hidden key
//! as its last key row, workspace#552).

use fastmig_ondisk::class_object::{ClassObject, ConstraintKind, class_properties};
use fastmig_ondisk::value::{DecodeError, DecodeResult, SetElement, SetValue, Value};
use fastmig_ondisk::{Oid, VersionLine};

use crate::ct_rows::{Sink, int, obj, oid_seq, opt_text, text};
use crate::system_table::SystemTable;

/// `(family, is_unique, is_reverse, is_primary_key, is_foreign_key)` in catcls order.
const FAMILIES: [(ConstraintKind, i32, i32, i32, i32); 6] = [
    (ConstraintKind::PrimaryKey, 1, 0, 1, 0),
    (ConstraintKind::Unique, 1, 0, 0, 0),
    (ConstraintKind::ReverseUnique, 1, 1, 0, 0),
    (ConstraintKind::Index, 0, 0, 0, 0),
    (ConstraintKind::ReverseIndex, 0, 1, 0, 0),
    (ConstraintKind::ForeignKey, 0, 0, 0, 1),
];

fn err(what: String) -> DecodeError {
    DecodeError { at: 0, what }
}

fn elements(v: &Value) -> DecodeResult<&[SetElement]> {
    match v {
        Value::Set(SetValue { elements, .. }) => Ok(elements),
        v => Err(err(format!("expected a sequence, got {v:?}"))),
    }
}

/// Prefix lengths of an index's keys from its optional element; `None` when it has none.
fn prefix_lengths(name: &[u8], optional: &Value) -> DecodeResult<Option<Vec<Value>>> {
    let entries = elements(optional)?;
    if entries
        .first()
        .is_some_and(|e| matches!(e.value, Value::Integer(_)))
    {
        return Ok(Some(entries.iter().map(|e| e.value.clone()).collect()));
    }
    for e in entries {
        let pair = elements(&e.value)?;
        match pair.first().map(|f| &f.value) {
            Some(Value::Varchar(f) | Value::Char(f)) if f == b"*PLID*" => {
                let info = pair
                    .get(1)
                    .ok_or_else(|| err("*PLID* entry without lengths".into()))?;
                return Ok(Some(
                    elements(&info.value)?
                        .iter()
                        .map(|e| e.value.clone())
                        .collect(),
                ));
            }
            Some(Value::Varchar(f) | Value::Char(f)) if f == b"*FP*" || f == b"*FI*" => {
                return Err(err(format!(
                    "filter or function index {} in the target catalog (workspace#486 defers them)",
                    String::from_utf8_lossy(name)
                )));
            }
            _ => {}
        }
    }
    Ok(None)
}

/// The `_db_index` rows of class `c`; returns the `indexes` column value.
pub(crate) fn index_rows(
    s: &mut Sink,
    class_row: Oid,
    c: &ClassObject,
    now: &Value,
) -> DecodeResult<Value> {
    let props = class_properties(&c.properties, s.line)?;
    let attr_name = |id: i32| -> DecodeResult<Value> {
        c.attributes
            .iter()
            .find(|a| a.id == id)
            .map(|a| text(&a.name))
            .ok_or_else(|| {
                err(format!(
                    "index key attribute id {id} is not an instance attribute"
                ))
            })
    };
    let mut oids = Vec::new();
    for (kind, is_unique, is_reverse, is_pk, is_fk) in FAMILIES {
        for k in props.constraints.iter().filter(|k| k.kind == kind) {
            let me = (s.alloc)(SystemTable::DbIndex);
            oids.push(me);
            // catcls requires the optional element on every index that is neither a PK nor an FK
            let prefix = if is_pk == 0 && is_fk == 0 {
                let opt = k.optional.as_ref().ok_or_else(|| {
                    err(format!(
                        "index {} has no optional element",
                        String::from_utf8_lossy(&k.name)
                    ))
                })?;
                prefix_lengths(&k.name, opt)?
            } else {
                None
            };
            let mut key_oids = Vec::with_capacity(k.keys.len());
            for (order, (id, asc_desc)) in k.keys.iter().enumerate() {
                let ko = (s.alloc)(SystemTable::DbIndexKey);
                key_oids.push(ko);
                let prefix = prefix
                    .as_ref()
                    .and_then(|p| p.get(order).cloned())
                    .unwrap_or(int(-1));
                s.emit(
                    SystemTable::DbIndexKey,
                    ko,
                    vec![
                        ("index_of", Value::Object(me)),
                        ("key_attr_name", attr_name(*id)?),
                        ("key_order", int(order as i32)),
                        ("asc_desc", int(*asc_desc)),
                        ("key_prefix_length", prefix),
                    ],
                );
            }
            // an 11.4 target lists a DEDUPLICATE index's hidden key last, named `_dedup_NN` (release/11.4
            // `catcls_convert_attr_id_to_name`, `dk_get_deduplicate_key_attr_name`); develop has no such key
            // shortcut: its prefix length is written -1, as for every key of an index without prefix lengths; check the
            // engine row once an 11.3 source with a DEDUPLICATE prefix index exists
            if let Some(d) = k.deduplicate_key.filter(|_| s.line != VersionLine::Develop) {
                let ko = (s.alloc)(SystemTable::DbIndexKey);
                s.emit(
                    SystemTable::DbIndexKey,
                    ko,
                    vec![
                        ("index_of", Value::Object(me)),
                        (
                            "key_attr_name",
                            text(format!("_dedup_{:02}", d.level).as_bytes()),
                        ),
                        ("key_order", int(key_oids.len() as i32)),
                        ("asc_desc", int(d.desc as i32)),
                        ("key_prefix_length", int(-1)),
                    ],
                );
                key_oids.push(ko);
            }
            let fk = k.foreign_key.as_ref();
            s.emit(
                SystemTable::DbIndex,
                me,
                vec![
                    ("class_of", Value::Object(class_row)),
                    ("index_name", text(&k.name)),
                    ("is_unique", int(is_unique)),
                    ("key_count", int(key_oids.len() as i32)),
                    ("key_attrs", oid_seq(&key_oids)),
                    ("is_reverse", int(is_reverse)),
                    ("is_primary_key", int(is_pk)),
                    ("is_foreign_key", int(is_fk)),
                    ("have_function", int(0)),
                    ("status", int(k.status)),
                    (
                        "referential_class",
                        fk.map_or(Value::Null, |f| obj(f.ref_class)),
                    ),
                    (
                        "delete_rule",
                        fk.map_or(Value::Null, |f| int(f.delete_action)),
                    ),
                    (
                        "update_rule",
                        fk.map_or(Value::Null, |f| int(f.update_action)),
                    ),
                    (
                        "referential_match_option",
                        fk.map_or(Value::Null, |f| f.match_option.map_or(Value::Null, int)),
                    ),
                    ("index_type", k.index_type.map_or(Value::Null, int)),
                    ("options", k.options.map_or(Value::Null, int)),
                    ("comment", opt_text(k.comment.as_deref())),
                    ("created_time", now.clone()),
                    ("updated_time", now.clone()),
                ],
            );
        }
    }
    Ok(oid_seq(&oids))
}
