//! Takes the expression stream out of a partitioned class's packed `pvalues`.
//!
//! Input: the packed `pvalues` value of the master partition element (`pname` NULL) and its release line.
//! Output: the stream bytes, `8 + body_size` long (an 11.4 CHAR container pads them with spaces), or `None` when the
//! sequence has no stream element.
//! Source: engine `execute_schema.c` (release/11.4 @46212adcd :15501-15511 `db_make_char`, develop @5b1867174
//! :16413-16452 `db_make_varchar`: element 0 key column name, 1 hash size, 2 the stream); R7 § 2.1, § 2.4 steps 1-2.

use crate::VersionLine;
use crate::value::{DecodeError, DecodeResult, OrReader, Value, decode_tagged};

/// The stream held in element 2 of a master partition's `pvalues`.
pub fn stream_of_pvalues(line: VersionLine, packed: &[u8]) -> DecodeResult<Option<Vec<u8>>> {
    if packed.is_empty() {
        return Ok(None);
    }
    let (_, v) = decode_tagged(line, &mut OrReader::new(packed), Some(packed.len()))?;
    let Value::Set(s) = v else {
        return Ok(None);
    };
    let raw = match s.elements.get(2).map(|e| &e.value) {
        Some(Value::Char(b) | Value::Varchar(b)) => b,
        _ => return Ok(None),
    };
    let err = |what: &str| DecodeError {
        at: 0,
        what: what.into(),
    };
    let body = raw
        .get(4..8)
        .map(|b| i32::from_be_bytes(b.try_into().unwrap()))
        .ok_or_else(|| err("partition stream shorter than its header"))?;
    let len = 8 + usize::try_from(body).map_err(|_| err("negative stream body size"))?;
    raw.get(..len)
        .map(|s| Some(s.to_vec()))
        .ok_or_else(|| err("partition stream shorter than its body size"))
}
