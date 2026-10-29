//! Transcodes a partition expression's serialized XASL stream from one release line's layout to another's
//! (R7 § 2.4: opcode renumbering, CHAR/NUMERIC constants re-encoded, shared nodes kept).
//!
//! Input: the stream bytes (`[int 0][int body_size]` + body, `8 + body_size` bytes) taken out of the master
//! partition's `pvalues[2]` container. Output: the stream in `to`'s layout. The container itself (11.4 CHAR,
//! develop VARCHAR) is the caller's (`catalog::class_rewrite`).
//! Source: R7 § 2 (workspace#487); the transcoder is ticket P1g workspace#497.

use crate::VersionLine;
use crate::value::{DecodeError, DecodeResult};

/// Not implemented yet: filled by P1g (workspace#497).
pub fn transcode_partition_stream(
    from: VersionLine,
    to: VersionLine,
    stream: &[u8],
) -> DecodeResult<Vec<u8>> {
    if from == to {
        return Ok(stream.to_vec());
    }
    Err(DecodeError {
        at: 0,
        what: format!(
            "partition stream transcoding {from:?} -> {to:?} ({} B) is not implemented yet (workspace#497)",
            stream.len()
        ),
    })
}
