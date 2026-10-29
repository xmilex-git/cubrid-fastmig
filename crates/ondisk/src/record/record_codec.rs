//! Decodes a heap record into its attribute values under a layout, and writes values back as a record in either
//! version's encoding.
//!
//! Input: record bytes + [`RecordLayout`] (decode), or header + layout + values (encode). Output: [`DecodedRecord`]
//! (header and one value per attribute, storage order), or the record bytes.
//! Layout: `[header][variable offset table: (n_var + 1) × offset_size, 4-aligned][fixed area][bound bits:
//! 4 × ceil(n_fixed / 32) B][variable values]`; offsets count from the end of the header; a zero-length variable
//! entry is NULL; a cleared bound bit (byte `i / 8`, bit `i % 8`) makes fixed attribute `i` NULL.
//! Source: engine `src/base/object_representation.h:496-690` (`OR_VAR_*`, `OR_GET_BOUND_BIT*`),
//! `src/storage/heap_file.c` `heap_attrinfo_transform_columns_to_disk`, `_fixed_to_disk`, `_variable_to_disk`,
//! `heap_attrinfo_get_record_header_size` (develop @5b1867174 :12255-12760; release/11.4 @46212adcd same rules);
//! R2 § 1.7, § 3 R7.

use crate::VersionLine;
use crate::record::layout::RecordLayout;
use crate::record::mvcc_header::RecordHeader;
use crate::value::codec::{decode_value, encode_data};
use crate::value::db_value::Value;
use crate::value::or_buf::{DecodeResult, OrReader, OrWriter};

/// Engine `OR_MAX_BYTE` / `OR_MAX_SHORT`: the largest record (header + table + payload) a 1- or 2-byte offset table
/// may describe.
const MAX_BYTE: usize = 127;
const MAX_SHORT: usize = 32767;
/// Engine `OR_MVCC_INSERT_HEADER_SIZE` / `OR_NON_MVCC_HEADER_SIZE`: the header the offset-size rule assumes.
const MVCC_INSERT_HEADER: usize = 16;
const NON_MVCC_HEADER: usize = 8;

/// A record's header and attribute values (storage order, NULL as [`Value::Null`]).
#[derive(Clone, Debug, PartialEq)]
pub struct DecodedRecord {
    pub header: RecordHeader,
    pub values: Vec<Value>,
}

fn var_table_size(n_var: usize, offset_size: usize) -> usize {
    if n_var == 0 {
        0
    } else {
        (offset_size * (n_var + 1)).div_ceil(4) * 4
    }
}

fn bound_bytes(n_fixed: usize) -> usize {
    n_fixed.div_ceil(32) * 4
}

/// Entry `at` of the variable offset table: an `size`-byte big-endian offset, negative read as 0.
fn read_offset(rec: &[u8], at: usize, size: usize) -> DecodeResult<usize> {
    let mut r = OrReader::new(rec);
    r.skip(at)?;
    Ok(match size {
        1 => r.u8()? as usize,
        2 => r.i16()?.max(0) as usize,
        _ => r.i32()?.max(0) as usize,
    })
}

/// Decodes `rec` under `layout` as written by `version`.
pub fn decode_record(
    version: VersionLine,
    layout: &RecordLayout,
    rec: &[u8],
) -> DecodeResult<DecodedRecord> {
    decode_record_attrs(version, layout, rec, |_| true)
}

/// [`decode_record`] of the attributes `want` names (by attribute id) only; the others read as [`Value::Null`]. For
/// a source whose other values this tool cannot decode yet (10.2 LZO strings, workspace#520).
pub fn decode_record_attrs(
    version: VersionLine,
    layout: &RecordLayout,
    rec: &[u8],
    want: impl Fn(i32) -> bool,
) -> DecodeResult<DecodedRecord> {
    let header = RecordHeader::read(rec)?;
    let hs = header.size();
    let n_var = layout.n_variable();
    let os = header.offset_size;
    let table = var_table_size(n_var, os);
    let fixed_start = hs + table;
    let (locs, fixed_length) = layout.fixed_locations(version)?;
    let bits_at = fixed_start + fixed_length;
    let mut r = OrReader::new(rec);
    if bits_at + bound_bytes(layout.n_fixed) > rec.len() {
        return Err(r.error(format!(
            "record of {} B too short for its fixed area",
            rec.len()
        )));
    }
    let mut values = Vec::with_capacity(layout.attrs.len());
    for (i, a) in layout.attrs[..layout.n_fixed].iter().enumerate() {
        let bound = !header.has_bound_bits || rec[bits_at + i / 8] & (1 << (i % 8)) != 0;
        if !bound || !want(a.id) {
            values.push(Value::Null);
            continue;
        }
        r = OrReader::new(rec);
        r.skip(fixed_start + locs[i])?;
        values.push(decode_value(
            version,
            &mut r,
            Some(&a.domain),
            a.domain.disk_size(version),
        )?);
    }
    for (j, a) in layout.attrs[layout.n_fixed..].iter().enumerate() {
        let start = hs + read_offset(rec, hs + j * os, os)?;
        let end = hs + read_offset(rec, hs + (j + 1) * os, os)?;
        if end < start || end > rec.len() {
            return Err(r.error(format!(
                "variable attribute {} spans {start}..{end} of {}",
                a.id,
                rec.len()
            )));
        }
        if end == start || !want(a.id) {
            values.push(Value::Null);
            continue;
        }
        r = OrReader::new(rec);
        r.skip(start)?;
        values.push(decode_value(
            version,
            &mut r,
            Some(&a.domain),
            Some(end - start),
        )?);
    }
    Ok(DecodedRecord { header, values })
}

/// Writes `values` (storage order of `layout`) as a record in `version`'s encoding. `header` supplies the repid,
/// CHN and MVCC fields; the bound-bit flag and offset size are recomputed. The offset size follows the engine's
/// rule for a class that is (`mvcc_class`) or is not MVCC.
pub fn encode_record(
    version: VersionLine,
    layout: &RecordLayout,
    header: &RecordHeader,
    values: &[Value],
    mvcc_class: bool,
) -> DecodeResult<Vec<u8>> {
    assert_eq!(values.len(), layout.attrs.len(), "one value per attribute");
    let (locs, fixed_length) = layout.fixed_locations(version)?;
    let n_var = layout.n_variable();
    let n_fixed = layout.n_fixed;

    let mut fixed = vec![0u8; fixed_length];
    let mut bits = vec![0u8; bound_bytes(n_fixed)];
    for (i, a) in layout.attrs[..n_fixed].iter().enumerate() {
        if values[i].is_null() {
            continue;
        }
        let mut w = OrWriter::new();
        encode_data(version, &mut w, &a.domain, &values[i]);
        fixed[locs[i]..locs[i] + w.len()].copy_from_slice(&w.buf);
        bits[i / 8] |= 1 << (i % 8);
    }
    // the variable area starts 4-aligned (header, table, fixed area and bound bits are all multiples of 4),
    // so values aligned relative to it are aligned relative to the record
    let mut var = OrWriter::new();
    let mut offsets = Vec::with_capacity(n_var + 1);
    for (j, a) in layout.attrs[n_fixed..].iter().enumerate() {
        offsets.push(var.len());
        let v = &values[n_fixed + j];
        if !v.is_null() {
            encode_data(version, &mut var, &a.domain, v);
        }
    }
    offsets.push(var.len());

    // `heap_attrinfo_get_record_header_size`
    let assumed = if mvcc_class {
        MVCC_INSERT_HEADER
    } else {
        NON_MVCC_HEADER
    };
    let payload = fixed_length + var.len();
    let fits = |os: usize, max: usize| {
        assumed + var_table_size(n_var, os) + bound_bytes(n_fixed) + payload <= max
    };
    let os = if fits(1, MAX_BYTE) {
        1
    } else if fits(2, MAX_SHORT) {
        2
    } else {
        4
    };

    let mut h = *header;
    h.offset_size = os;
    h.has_bound_bits = n_fixed > 0;
    let mut w = OrWriter::new();
    h.write(&mut w);
    let base = fixed_length + bound_bytes(n_fixed) + var_table_size(n_var, os);
    if n_var > 0 {
        let table_start = w.len();
        for off in &offsets {
            let v = base + off;
            match os {
                1 => w.u8(v as u8),
                2 => w.i16(v as i16),
                _ => w.i32(v as i32),
            }
        }
        let pad = var_table_size(n_var, os) - (w.len() - table_start);
        w.zeros(pad);
    }
    w.bytes(&fixed);
    w.bytes(&bits);
    w.bytes(&var.buf);
    Ok(w.buf)
}
