//! Transcodes a partition expression's serialized XASL stream from one release line's layout to another's
//! (R7 § 2.4: opcode renumbering, CHAR/NUMERIC constants re-encoded, shared nodes kept).
//!
//! Input: the stream bytes (`[int 0][int body_size]` + body, `8 + body_size` bytes) taken out of the master
//! partition's `pvalues[2]` container. Output: the stream in `to`'s layout. The container itself (11.4 CHAR,
//! develop VARCHAR) is the caller's (`catalog::class_rewrite`).
//! Source: R7 § 2 (workspace#487), R13 (workspace#523, 10.2 numbers); tickets P1g workspace#497, T17
//! workspace#540. The stream is parsed into a node tree ([`read`]), mapped ([`to_develop`]) and written again
//! where the engine's writer would place each node ([`write`]), so the offsets are recomputed for the new value
//! lengths.

pub mod char_result;
pub mod container;
pub mod opcode;
pub mod read;
pub mod to_develop;
pub mod tree;
pub mod write;

pub use read::parse;
pub use tree::StreamTree;
pub use write::{serialize, serialize_with_mask};

use crate::VersionLine;
use crate::value::{DecodeError, DecodeResult};

/// The stream in `to`'s layout. Same line: the bytes unchanged. 11.4 or 10.2 → develop: parse, map, write. Other
/// pairs are refused. A stream the parser cannot read (a node type a partition expression cannot hold, a bad offset)
/// is an error naming the offset.
pub fn transcode_partition_stream(
    from: VersionLine,
    to: VersionLine,
    stream: &[u8],
) -> DecodeResult<Vec<u8>> {
    if from == to {
        return Ok(stream.to_vec());
    }
    if !matches!(
        (from, to),
        (
            VersionLine::V10_2 | VersionLine::V11_4,
            VersionLine::Develop
        )
    ) {
        return Err(DecodeError {
            at: 0,
            what: format!("partition stream transcoding {from:?} -> {to:?} is not supported"),
        });
    }
    let mut tree = parse(from, stream)?;
    to_develop::map_tree(from, &mut tree)?;
    let out = serialize(to, &tree);
    // self-check (R7 § 2.4 step 7): the written stream reads back as the same tree
    if parse(to, &out)?.canonical() != tree.canonical() {
        return Err(DecodeError {
            at: 0,
            what: "transcoded partition stream does not read back as the tree it was written from"
                .into(),
        });
    }
    Ok(out)
}
