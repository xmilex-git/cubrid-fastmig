//! Packed collections (SET, MULTISET, SEQUENCE): header word, element count, offset table or bound bits, optional
//! domain, and the elements (tagged when the element domain is not a single type).
//!
//! Input: OR bytes at a cursor and the column's domain when the set does not carry one. Output: [`SetValue`], or
//! the packed bytes. The layout flags are recomputed from the domain on encode (engine `or_packed_set_info`), so a
//! set whose element type became variable in develop (CHAR, NUMERIC) gets an offset table (R2 § 3 R7.5).
//! Source: engine `src/object/object_representation.c` `or_packed_set_info`, `or_put_set_header`,
//! `or_get_set_header`, `or_put_set`, `or_get_set` (develop @5b1867174 :3880-4560; same in release/11.4
//! @46212adcd); `src/base/object_representation.h:698-760` (header bits, offsets).

use crate::VersionLine;
use crate::value::codec::{decode_tagged, decode_value, encode_data, encode_tagged};
use crate::value::db_type::DbType;
use crate::value::db_value::{SetElement, SetValue, Value};
use crate::value::domain::Domain;
use crate::value::or_buf::{DecodeResult, OrReader, OrWriter};
use crate::value::packed_domain::{decode_packed_domain, encode_packed_domain};

const TYPE_MASK: u32 = 0xFF;
const BOUND_BIT: u32 = 0x10000;
const VARIABLE_BIT: u32 = 0x20000;
const DOMAIN_BIT: u32 = 0x40000;
const TAG_BIT: u32 = 0x80000;

fn bound_bit_bytes(n: usize) -> usize {
    n.div_ceil(32) * 4
}

/// Engine `OR_VAR_TABLE_SIZE`: (n + 1) 4-byte offsets, none for an empty set.
fn offset_table_bytes(n: usize) -> usize {
    if n == 0 { 0 } else { 4 * (n + 1) }
}

/// The single element domain of a homogeneous set, with its fixed size (`None` = variable).
fn homogeneous(version: VersionLine, d: Option<&Domain>) -> Option<(&Domain, Option<usize>)> {
    match d.map(|d| d.set_domain.as_slice()) {
        Some([e]) => Some((e, e.disk_size(version))),
        _ => None,
    }
}

/// Reads one packed set (engine `or_get_set`). `domain` is the column's domain, used when the set header has none.
pub fn decode_set(
    version: VersionLine,
    r: &mut OrReader,
    domain: Option<&Domain>,
) -> DecodeResult<SetValue> {
    let start = r.pos();
    let header = r.u32()?;
    let count = r.i32()?;
    if count < 0 {
        return Err(r.error(format!("set element count {count}")));
    }
    let count = count as usize;
    let id = header & TYPE_MASK;
    let ty = DbType::from_id(version, id).ok_or_else(|| r.error(format!("set type id {id}")))?;
    let mut offsets = Vec::new();
    let mut bound = Vec::new();
    if count > 0 {
        if header & VARIABLE_BIT != 0 {
            for _ in 0..=count {
                offsets.push(r.i32()?);
            }
        } else if header & BOUND_BIT != 0 {
            bound = r.bytes(bound_bit_bytes(count))?.to_vec();
        }
    }
    let own_domain = if header & DOMAIN_BIT != 0 {
        let _size = r.i32()?;
        decode_packed_domain(version, r)?.into_iter().next()
    } else {
        None
    };
    let set_domain = own_domain.as_ref().or(domain);
    let element = homogeneous(version, set_domain);
    let tagged = header & TAG_BIT != 0;
    let mut elements = Vec::with_capacity(count);
    for i in 0..count {
        let size = if !offsets.is_empty() {
            Some((offsets[i + 1] - offsets[i]).max(0) as usize)
        } else if !bound.is_empty() {
            let word =
                u32::from_be_bytes(bound[(i / 32) * 4..(i / 32) * 4 + 4].try_into().unwrap());
            let fixed = element.and_then(|(_, s)| s).unwrap_or(0);
            if word & (1 << (i & 31)) == 0 {
                r.skip(fixed)?;
                Some(0)
            } else {
                Some(fixed)
            }
        } else {
            None
        };
        let el = if size == Some(0) {
            SetElement {
                tag: None,
                value: Value::Null,
            }
        } else if tagged {
            let (tag, value) = decode_tagged(version, r, size)?;
            SetElement {
                tag: Some(tag),
                value,
            }
        } else {
            let ed = element
                .map(|(e, _)| e)
                .ok_or_else(|| r.error("untagged set element without a domain"))?;
            SetElement {
                tag: None,
                value: decode_value(version, r, Some(ed), size)?,
            }
        };
        elements.push(el);
    }
    let used = r.pos() - start;
    r.skip((4 - (used & 3)) & 3)?;
    Ok(SetValue {
        ty,
        domain: own_domain,
        elements,
    })
}

/// Writes one packed set (engine `or_put_set` with `include_domain` when the set carries a domain). Layout flags
/// come from the set's domain (its own, else `domain`); tagged elements need their tag.
pub fn encode_set(version: VersionLine, w: &mut OrWriter, domain: Option<&Domain>, s: &SetValue) {
    let set_domain = s.domain.as_ref().or(domain);
    let element = homogeneous(version, set_domain);
    let bound_bits = matches!(element, Some((_, Some(_))));
    let offset_table = !bound_bits;
    let tags = element.is_none();
    let n = s.elements.len();
    let start = w.len();
    let mut header = set_domain.map_or(s.ty, |d| d.ty).id() as u32;
    header |= if offset_table {
        VARIABLE_BIT
    } else {
        BOUND_BIT
    };
    if s.domain.is_some() {
        header |= DOMAIN_BIT;
    }
    if tags {
        header |= TAG_BIT;
    }
    w.u32(header);
    w.i32(n as i32);
    let table_at = w.len();
    if n > 0 {
        w.zeros(if offset_table {
            offset_table_bytes(n)
        } else {
            bound_bit_bytes(n)
        });
    }
    if let Some(d) = &s.domain {
        let mut dw = OrWriter::new();
        encode_packed_domain(version, &mut dw, std::slice::from_ref(d), false, false);
        w.i32(dw.len() as i32);
        w.bytes(&dw.buf);
    }
    for (i, el) in s.elements.iter().enumerate() {
        let el_start = w.len();
        if offset_table {
            w.patch_i32(table_at + 4 * i, (el_start - start) as i32);
        }
        match (element, &el.value) {
            (Some((_, Some(fixed))), Value::Null) => w.zeros(fixed),
            (Some((ed, _)), v) if !v.is_null() => {
                encode_data(version, w, ed, v);
                if bound_bits {
                    let at = table_at + (i / 32) * 4;
                    let word =
                        u32::from_be_bytes(w.buf[at..at + 4].try_into().unwrap()) | 1 << (i & 31);
                    w.buf[at..at + 4].copy_from_slice(&word.to_be_bytes());
                }
            }
            // collapsed NULL in an untagged offset-table set
            (Some(_), _) => {}
            (None, v) => {
                let null_tag = Domain::builtin(version, DbType::Null);
                let tag = el.tag.as_ref().unwrap_or(&null_tag);
                encode_tagged(version, w, tag, v);
            }
        }
        if offset_table {
            let pad = (4 - ((w.len() - el_start) & 3)) & 3;
            w.zeros(pad);
        }
    }
    if offset_table && n > 0 {
        w.patch_i32(table_at + 4 * n, (w.len() - start) as i32);
    }
    let pad = (4 - ((w.len() - start) & 3)) & 3;
    w.zeros(pad);
}
