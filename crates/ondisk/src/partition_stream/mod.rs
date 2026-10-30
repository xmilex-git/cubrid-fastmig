//! Transcodes a partition expression's serialized XASL stream from one release line's layout to another's
//! (R7 § 2.4: opcode renumbering, CHAR/NUMERIC constants re-encoded, shared nodes kept).
//!
//! Input: the stream bytes (`[int 0][int body_size]` + body, `8 + body_size` bytes) taken out of the master
//! partition's `pvalues[2]` container. Output: the stream in `to`'s layout. The container itself (11.4 CHAR,
//! develop VARCHAR) is the caller's (`catalog::class_rewrite`, `catalog::class_rewrite_11_4`).
//! Source: R7 § 2 (workspace#487), R13 (workspace#523, 10.2 numbers); tickets P1g workspace#497, T17
//! workspace#540, T24 workspace#552 (11.4 target). The stream is parsed into a node tree ([`read`]), mapped
//! ([`to_develop`], [`to_11_4`]) and written again where the engine's writer would place each node ([`write`]), so
//! the offsets are recomputed for the new value lengths.

pub mod char_result;
pub mod container;
pub mod lang_flag;
pub mod opcode;
pub mod read;
pub mod to_11_4;
pub mod to_develop;
pub mod tree;
pub mod write;

pub use read::parse;
pub use tree::StreamTree;
pub use write::{serialize, serialize_with_mask};

use crate::VersionLine;
use crate::value::{DecodeError, DecodeResult};

/// The stream in `to`'s layout. develop → develop: the bytes unchanged. 11.4 or 10.2 → develop and 10.2 or 11.4 →
/// 11.4: parse, map, write; the 11.4 target is only reached from a source before 11.4 (an 11.2/11.3 stream is read
/// with the 11.4 line and still needs the language flag parity, workspace#552). Other pairs are refused. A stream the
/// parser cannot read (a node type a partition expression cannot hold, a bad offset) is an error naming the offset.
pub fn transcode_partition_stream(
    from: VersionLine,
    to: VersionLine,
    stream: &[u8],
) -> DecodeResult<Vec<u8>> {
    let tree = match (from, to) {
        (VersionLine::Develop, VersionLine::Develop) => return Ok(stream.to_vec()),
        (VersionLine::V10_2 | VersionLine::V11_4, VersionLine::Develop) => {
            let mut tree = parse(from, stream)?;
            to_develop::map_tree(from, &mut tree)?;
            tree
        }
        (VersionLine::V10_2 | VersionLine::V11_4, VersionLine::V11_4) => {
            let mut tree = parse(from, stream)?;
            to_11_4::map_tree(from, &mut tree)?;
            tree
        }
        _ => {
            return Err(DecodeError {
                at: 0,
                what: format!("partition stream transcoding {from:?} -> {to:?} is not supported"),
            });
        }
    };
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
