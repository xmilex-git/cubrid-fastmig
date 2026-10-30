//! The index key of one heap record: the key attributes' values encoded as the key domain's image.
//!
//! Input: a [`KeyDomain`], the record's layout and decoded values, and the index's [`Constraint`] (its key
//! attributes in key order; a primary key's columns must not be NULL). Output: [`ExtractedKey`] — the key image, an all-NULL key (the engine stores
//! none for it), or a NULL in a primary key column (a record the source should not hold).
//! Function and filter indexes are refused: the tool does not build them (workspace#486).
//! Values must already be in the key domain's version (map 11.4 values with
//! [`crate::value::value_to_develop`] before building a develop key).
//! An 11.4 key of a DEDUPLICATE index ends in the hidden SHORT column `pageid % 2^level` of the row's home OID; a
//! row whose real key columns are all NULL has no entry (R16 workspace#532, T14 workspace#537).
//! Source: engine `src/storage/heap_file.c` `heap_attrvalue_get_key` / `heap_midxkey_key_get` (key columns from the
//! attribute values in index order), `src/storage/btree.c` (an all-NULL key is not inserted:
//! `btree_insert`'s `DB_IS_NULL (key)` and `btree_multicol_key_is_null`), develop @5b1867174;
//! release/11.4 `deduplicate_key.c:dk_get_deduplicate_key_value`.

use crate::class_object::{Constraint, ConstraintKind};
use crate::ids::Oid;
use crate::record::layout::RecordLayout;
use crate::value::db_value::Value;
use crate::value::key_domain::KeyDomain;
use crate::value::or_buf::{DecodeError, DecodeResult};
use crate::version_line::VersionLine;

/// What a record contributes to an index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExtractedKey {
    /// The key image to insert with the record's OID.
    Key(Vec<u8>),
    /// Every key column is NULL: the index holds no entry for the record.
    AllNull,
    /// A primary key column is NULL (zero-based key column).
    NullInPrimaryKey(usize),
}

/// The key of the record `oid` (its home OID) whose `values` follow `layout`'s attribute order.
pub fn extract_key(
    kd: &KeyDomain,
    layout: &RecordLayout,
    values: &[Value],
    index: &Constraint,
    oid: Oid,
) -> DecodeResult<ExtractedKey> {
    if index.has_filter || index.has_function {
        return Err(DecodeError {
            at: 0,
            what: format!(
                "index {} is a filter or function index",
                String::from_utf8_lossy(&index.name)
            ),
        });
    }
    let key_attrs: Vec<i32> = index.keys.iter().map(|(id, _)| *id).collect();
    let primary_key = index.kind == ConstraintKind::PrimaryKey;
    // only an 11.4 key domain carries the hidden column; a develop one was built without it
    let hidden = index
        .deduplicate_key
        .filter(|_| kd.version == VersionLine::V11_4);
    if key_attrs.len() + usize::from(hidden.is_some()) != kd.n_columns() {
        return Err(DecodeError {
            at: 0,
            what: format!(
                "{} key attributes for a {}-column key",
                key_attrs.len() + usize::from(hidden.is_some()),
                kd.n_columns()
            ),
        });
    }
    let mut key = Vec::with_capacity(key_attrs.len());
    for (i, id) in key_attrs.iter().enumerate() {
        let pos = layout
            .attrs
            .iter()
            .position(|a| a.id == *id)
            .ok_or_else(|| DecodeError {
                at: 0,
                what: format!(
                    "key attribute {id} is not in representation {}",
                    layout.repid
                ),
            })?;
        let v = &values[pos];
        if primary_key && v.is_null() {
            return Ok(ExtractedKey::NullInPrimaryKey(i));
        }
        key.push(v.clone());
    }
    if key.iter().all(Value::is_null) {
        return Ok(ExtractedKey::AllNull);
    }
    // a single-column key is NULL or not; only a MIDXKEY can hold some NULL columns
    if !kd.midxkey && key[0].is_null() {
        return Ok(ExtractedKey::AllNull);
    }
    if let Some(h) = hidden {
        // engine `(short) (pageid % (1u << level))`, level 1..14
        key.push(Value::Short((oid.pageid as u32 % (1u32 << h.level)) as i16));
    }
    Ok(ExtractedKey::Key(kd.encode_key(&key)))
}
