//! `KeyOrder` for the value codec's parsed key domain (workspace#491 `KeyDomain`), so the builder sorts by the
//! engine's key order without knowing types or collations.
//!
//! Input: key images of one index. Output: what `KeyDomain` says about them.
//! Source: `fastmig_ondisk::value::KeyDomain` (`compare`, `common_columns`, `n_columns`, `radix_words` in the
//! format of `KeyOrder::radix_words`). The impl lives here because the trait does.

use crate::key_order::KeyOrder;
use fastmig_ondisk::value::KeyDomain;
use std::cmp::Ordering;

impl KeyOrder for KeyDomain {
    fn compare(&self, a: &[u8], b: &[u8]) -> Ordering {
        KeyDomain::compare(self, a, b)
    }

    fn columns(&self) -> usize {
        self.n_columns()
    }

    fn common_columns(&self, a: &[u8], b: &[u8]) -> usize {
        KeyDomain::common_columns(self, a, b)
    }

    fn radix_words(&self, key: &[u8], out: &mut Vec<u64>) {
        KeyDomain::radix_words(self, key, out)
    }

    fn leading_null_columns(&self, key: &[u8]) -> usize {
        if !self.midxkey {
            return 0;
        }
        // MIDXKEY null bitmap: bit i % 8 of byte i / 8 set = column i present
        (0..self.n_columns())
            .take_while(|&i| key.get(i / 8).is_some_and(|b| b & (1 << (i % 8)) == 0))
            .count()
    }
}
