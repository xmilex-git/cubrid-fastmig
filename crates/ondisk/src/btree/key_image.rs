//! What a B-tree reader needs to know about a key image that only the key's domain can tell.
//!
//! Input: a key image (`pr_type->index_writeval` bytes, MIDXKEY for multi-column keys).
//! Output: its length, or the decode error of a damaged image.
//! Source: engine leaf records store no key length (`LEAF_RECORD_SIZE` 0, R6 § 3.3), readers take it from
//! the key domain's `index_readval` (develop 5b1867174). Implemented by the value codec (workspace#491) for a
//! parsed key domain.

use crate::value::DecodeResult;

/// Domain knowledge a leaf record reader needs. One value per index (one key domain).
pub trait KeyImage: Sync {
    /// Bytes the key image at the start of `bytes` takes; `bytes` may run past its end. Every column of the
    /// image is checked, so a key that passed this can be compared (`KeyDomain::compare`).
    fn image_len(&self, bytes: &[u8]) -> DecodeResult<usize>;
}
