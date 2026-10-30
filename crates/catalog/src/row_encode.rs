//! Encodes one generated catalog row as a heap record of its table (develop, or 11.4 on the 11.4 same-version path).
//!
//! Input: the table's class object (from the catalog template, or the target on the same-version path) and its
//! line, and a [`Row`] whose values are keyed by
//! attribute name. Output: the record bytes: the table's current repid, CHN 0, no MVCC fields (a target starts
//! with no vacuum residue, workspace#472 Notes), values in the class's storage order; a missing name is NULL.
//! Source: P1b `record::encode_record`; engine develop @682b080b2 `catalog_class.c:catcls_put_or_value_into_buffer`
//! (catcls writes CHN 0 and 4-byte variable offsets; the generic reader takes any size, but catcls on develop
//! optdebug asserts 2 or 4 when DDL rewrites the row, workspace#537 — R4 § 3.1 "readers take any size" corrected).

use fastmig_ondisk::VersionLine;
use fastmig_ondisk::class_object::ClassObject;
use fastmig_ondisk::record::{RecordHeader, RecordLayout, encode_record};
use fastmig_ondisk::value::{DecodeError, DecodeResult, Value};

use crate::class_rows::Row;

/// `table` is of `line`; the record is written in that line's value encodings.
pub fn encode_row(table: &ClassObject, line: VersionLine, row: &Row) -> DecodeResult<Vec<u8>> {
    let layout = RecordLayout::for_repid(line, table, table.repid as u32)?;
    let name_of = |id: i32| {
        table
            .attributes
            .iter()
            .find(|a| a.id == id)
            .map(|a| a.name.as_slice())
    };
    // a value under a name the table lacks is a generator defect, not a NULL
    for n in row.values.keys() {
        if !table.attributes.iter().any(|a| a.name == n.as_bytes()) {
            return Err(DecodeError {
                at: 0,
                what: format!("{} has no column {n}", String::from_utf8_lossy(&table.name)),
            });
        }
    }
    let values: Vec<Value> = layout
        .attrs
        .iter()
        .map(|a| {
            name_of(a.id)
                .and_then(|n| row.values.get(std::str::from_utf8(n).ok()?))
                .cloned()
                .unwrap_or(Value::Null)
        })
        .collect();
    let header = RecordHeader {
        repid: table.repid as u32,
        has_bound_bits: false,
        offset_size: 4, // catcls writes 4-byte offsets; the develop optdebug catcls reader asserts 2 or 4 (workspace#537)
        mvcc_flags: 0,
        chn: 0,
        insid: None,
        delid: None,
        prev_version_lsa: None,
    };
    encode_record(line, &layout, &header, &values, true)
}
