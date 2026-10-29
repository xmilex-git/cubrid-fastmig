//! The key ordering the builder sorts and splits by.
//!
//! Input: two key images of one index. Output: their order, the number of leading MIDXKEY columns they share,
//! and optionally radix words that order as the keys do.
//! Source: R6 § 4.2 R-SORT-1/2/4; engine `bt_load_cmp_key_bytes` (btree_load.c, develop 5b1867174), PR
//! CUBRID/cubrid#8209 `btree_sort_radix.hpp` RADIX_KEY_FORMAT. Implemented for a parsed key domain by the value
//! codec (workspace#491) — collation, NULL and DESC rules live there.

use std::cmp::Ordering;

pub trait KeyOrder: Sync {
    /// Key order: columns in turn, NULL < non-NULL, a DESC column reversed (R-SORT-1). Never looks at OIDs.
    fn compare(&self, a: &[u8], b: &[u8]) -> Ordering;

    /// Columns of the key (1 for a single-column key).
    fn columns(&self) -> usize;

    /// Leading columns on which `a` and `b` are equal (0..=columns()), for the partial-key statistics.
    fn common_columns(&self, a: &[u8], b: &[u8]) -> usize;

    /// Radix words of `key` into `out` (cleared by the caller): unsigned words whose lexicographic order agrees
    /// with `compare` as far as they go — equal words mean "compare decides". The default gives none, and the
    /// sort then runs on `compare` alone.
    fn radix_words(&self, _key: &[u8], _out: &mut Vec<u64>) {}
}
