//! What a B-tree reader needs to know about a key image that only the key's domain can tell.
//!
//! Input: a key image (`pr_type->index_writeval` bytes, MIDXKEY for multi-column keys).
//! Output: its length; for a prefix-compressed leaf, the full key rebuilt from the fence key.
//! Source: engine leaf records store no key length (`LEAF_RECORD_SIZE` 0, R6 § 3.3), readers take it from
//! the key domain's `index_readval`; prefix compression `btree_node_get_common_prefix`, `btree.c:16380-16449`
//! (develop 5b1867174). Implemented by the value codec (workspace#491) for a parsed key domain.

/// Domain knowledge a leaf record reader needs. One value per index (one key domain).
pub trait KeyImage: Sync {
    /// Bytes the key image at the start of `bytes` takes; `bytes` may run past its end.
    fn image_len(&self, bytes: &[u8]) -> usize;

    /// Full key of a leaf record on a page whose header has `common_prefix` = `ncols` > 0: the first `ncols`
    /// MIDXKEY columns come from `fence` (the page's lower fence key, a full image) and the rest from `stored`.
    fn restore_prefix(&self, fence: &[u8], stored: &[u8], ncols: usize) -> Vec<u8>;
}
