//! JSON values as the engine serializes them on disk: walked to find their length, with the string padding zeroed.
//!
//! Input: OR bytes at a cursor holding one serialized JSON document. Output: the document's bytes with every
//! string's alignment padding set to 0 (the engine leaves it uninitialized, so two equal documents can differ only
//! there). The format is the same in both versions.
//! Serialized form, all integers BE: a node is `type:i32` then — NULL(0)/UNKNOWN(1): nothing; INT(2): `is_uint:i32`,
//! `i32`; BIGINT(3): `is_uint:i32`, `i64`; DOUBLE(4): 8 B; STRING(5): `len:i32` (bytes incl. NUL), the bytes, zero
//! padding to 4; OBJECT(6): `count:i32`, then `count` × (key as STRING body without type, node); ARRAY(7):
//! `count:i32`, `count` nodes; BOOL(8): `i32`.
//! Source: engine `src/compat/db_json.cpp` `JSON_SERIALIZER::*` (`PackType`, `PackString` =
//! `or_put_string_aligned_with_length`), `db_json_deserialize_doc_internal`, `db_json_unpack_*`; `DB_JSON_TYPE`
//! in `src/compat/db_json.hpp:58-66` (develop @5b1867174; release/11.4 @46212adcd same values).

use crate::value::or_buf::{DecodeResult, OrReader};

/// Nesting limit: deeper documents are rejected rather than risk the stack.
const MAX_DEPTH: usize = 512;

fn string(r: &mut OrReader, out: &mut Vec<u8>) -> DecodeResult<()> {
    let len = r.i32()?;
    if len < 0 {
        return Err(r.error(format!("JSON string length {len}")));
    }
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(r.bytes(len as usize)?);
    let pad = (4 - (len as usize & 3)) & 3;
    r.skip(pad)?;
    out.resize(out.len() + pad, 0);
    Ok(())
}

fn node(r: &mut OrReader, out: &mut Vec<u8>, depth: usize) -> DecodeResult<()> {
    if depth > MAX_DEPTH {
        return Err(r.error("JSON nested too deep"));
    }
    let ty = r.i32()?;
    out.extend_from_slice(&ty.to_be_bytes());
    match ty {
        0 | 1 => {}
        2 | 8 => out.extend_from_slice(r.bytes(if ty == 2 { 8 } else { 4 })?),
        3 => out.extend_from_slice(r.bytes(12)?),
        4 => out.extend_from_slice(r.bytes(8)?),
        5 => string(r, out)?,
        6 | 7 => {
            let n = r.i32()?;
            if n < 0 {
                return Err(r.error(format!("JSON member count {n}")));
            }
            out.extend_from_slice(&n.to_be_bytes());
            for _ in 0..n {
                if ty == 6 {
                    string(r, out)?;
                }
                node(r, out, depth + 1)?;
            }
        }
        t => return Err(r.error(format!("JSON node type {t}"))),
    }
    Ok(())
}

/// Reads one serialized JSON document and returns its bytes with zeroed padding (same length as on disk).
pub fn read_json(r: &mut OrReader) -> DecodeResult<Vec<u8>> {
    let mut out = Vec::new();
    node(r, &mut out, 0)?;
    Ok(out)
}
