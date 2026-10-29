//! The two set shapes a class object stores: substructure sets (one element per attribute, domain, ...) and OID sets.
//!
//! Input: the bytes of one variable field holding a set. Output: the element slices, or the OIDs.
//! Source: engine `transform_cl.c:put_substructure_set` (header with offset table and common sub header; element
//! offsets relative to the set start, last one the end; an empty set keeps the header and the domain words),
//! `transform_cl.c:put_object_set` (header, domain length 4, one-word object domain, then 8-byte OIDs),
//! `object_representation.c:or_skip_set_header`, `object_representation.h` `OR_SET_*` bits; same in
//! release/11.4 @46212adcd and develop @5b1867174.

use super::or_be::read_oid;
use crate::Oid;
use crate::value::{DecodeResult, OrReader};

const OR_SET_BOUND_BIT: u32 = 0x10000;
const OR_SET_VARIABLE_BIT: u32 = 0x20000;
const OR_SET_DOMAIN_BIT: u32 = 0x40000;
const OR_SET_COMMON_SUB_BIT: u32 = 0x100000;
/// `OR_SUB_HEADER_SIZE`: class OID, repid, flags.
const OR_SUB_HEADER_SIZE: usize = 16;

/// The elements of a substructure set; an empty field (NULL list) gives no elements.
pub(crate) fn elements(field: &[u8]) -> DecodeResult<Vec<&[u8]>> {
    if field.is_empty() {
        return Ok(Vec::new());
    }
    let mut r = OrReader::new(field);
    let header = r.u32()?;
    let count = r.i32()?;
    if count == 0 {
        return Ok(Vec::new());
    }
    if header & OR_SET_VARIABLE_BIT == 0 || count < 0 {
        return Err(r.error(format!(
            "substructure set header {header:#x} count {count} has no offset table"
        )));
    }
    let mut offs = Vec::with_capacity(count as usize + 1);
    for _ in 0..=count {
        let o = r.i32()?;
        match usize::try_from(o) {
            Ok(o) if o <= field.len() && offs.last().is_none_or(|&p| o >= p) => offs.push(o),
            _ => {
                return Err(r.error(format!(
                    "element offset {o} out of order or past end {}",
                    field.len()
                )));
            }
        }
    }
    Ok(offs.windows(2).map(|w| &field[w[0]..w[1]]).collect())
}

/// Skips a set header the way `or_skip_set_header` does and returns the element count and a reader at element 0.
pub(crate) fn skip_header(field: &[u8]) -> DecodeResult<(usize, OrReader<'_>)> {
    let mut r = OrReader::new(field);
    let header = r.u32()?;
    let count = r.i32()?;
    let count =
        usize::try_from(count).map_err(|_| r.error(format!("negative set count {count}")))?;
    if header & OR_SET_VARIABLE_BIT != 0 {
        r.skip(4 * (count + 1))?;
    } else if header & OR_SET_BOUND_BIT != 0 {
        r.skip(count.div_ceil(32) * 4)?;
    }
    if header & OR_SET_DOMAIN_BIT != 0 {
        let len = r.i32()?;
        r.skip(
            usize::try_from(len).map_err(|_| r.error(format!("negative domain length {len}")))?,
        )?;
    }
    if header & OR_SET_COMMON_SUB_BIT != 0 {
        r.skip(OR_SUB_HEADER_SIZE)?;
    }
    Ok((count, r))
}

/// The OIDs of an OID set; an empty field gives none.
pub(crate) fn oids(field: &[u8]) -> DecodeResult<Vec<Oid>> {
    if field.is_empty() {
        return Ok(Vec::new());
    }
    let (count, mut r) = skip_header(field)?;
    (0..count).map(|_| read_oid(&mut r)).collect()
}
