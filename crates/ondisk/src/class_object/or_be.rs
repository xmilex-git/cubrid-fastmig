//! The variable offset table of one class-object structure (the record or a substructure) and its fixed fields.
//!
//! Input: the bytes of one structure, starting at its variable offset table. Output: its variable fields as slices,
//! its fixed fields as integers and OIDs, its strings.
//! Source: engine `object_representation.h` `OR_PUT_INT` (network order) and `OR_PUT_OID` (pageid 4, slotid 2,
//! volid 2), `transform_cl.c:read_var_table` (`BIG_VAR_OFFSET_SIZE` = 4: n + 1 offsets relative to the table
//! start, the last one the end), `transform_cl.c:get_string`; same in release/11.4 @46212adcd and develop
//! @5b1867174. Checked on T1 pages (workspace#492).

use crate::Oid;
use crate::value::{DecodeError, DecodeResult, OrReader, StringCompression, read_string};

/// Prefixes a decode error with the field it happened in, outermost last added.
pub(crate) fn within<T>(r: DecodeResult<T>, field: &str) -> DecodeResult<T> {
    r.map_err(|e| DecodeError {
        at: e.at,
        what: format!("{field}: {}", e.what),
    })
}

/// An OID written with `OR_PUT_OID` at the reader's position.
pub(crate) fn read_oid(r: &mut OrReader) -> DecodeResult<Oid> {
    let pageid = r.i32()?;
    let slotid = r.i16()?;
    let volid = r.i16()?;
    Ok(Oid::new(volid, pageid, slotid))
}

/// A string variable field: `None` when the field is empty (NULL string).
pub(crate) fn opt_string(field: &[u8], c: StringCompression) -> DecodeResult<Option<Vec<u8>>> {
    if field.is_empty() {
        return Ok(None);
    }
    read_string(&mut OrReader::new(field), c).map(Some)
}

/// One structure seen through its variable offset table.
pub(crate) struct Structure<'a> {
    bytes: &'a [u8],
    offsets: Vec<usize>,
}

impl<'a> Structure<'a> {
    /// `bytes` starts at the offset table and holds `nvars` variable fields.
    pub(crate) fn new(bytes: &'a [u8], nvars: usize) -> DecodeResult<Self> {
        let mut r = OrReader::new(bytes);
        let mut offsets = Vec::with_capacity(nvars + 1);
        for i in 0..=nvars {
            let o = r.i32()?;
            match usize::try_from(o) {
                Ok(o) if o <= bytes.len() && offsets.last().is_none_or(|&p| o >= p) => {
                    offsets.push(o)
                }
                _ => {
                    return Err(r.error(format!(
                        "var offset [{i}] = {o} out of order or past end {}",
                        bytes.len()
                    )));
                }
            }
        }
        Ok(Structure { bytes, offsets })
    }

    /// Variable field `i`; empty when the field is NULL.
    pub(crate) fn var(&self, i: usize) -> &'a [u8] {
        &self.bytes[self.offsets[i]..self.offsets[i + 1]]
    }

    /// A reader over the fixed area, which starts right after the table.
    pub(crate) fn fixed(&self) -> OrReader<'a> {
        OrReader::new(&self.bytes[4 * self.offsets.len()..self.offsets[0]])
    }

    /// End of the structure as the table records it.
    pub(crate) fn end(&self) -> usize {
        *self.offsets.last().unwrap()
    }
}
