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

    /// Leading NULL columns of `key` (0 for a single-column key, whose NULL keys are not in the tree). The engine's
    /// partial-key count starts from an all-NULL previous key, so these columns of the first key count as no new
    /// prefix (`btree_get_stats_midxkey`: saved values start NULL, compared with total order).
    fn leading_null_columns(&self, _key: &[u8]) -> usize {
        0
    }

    /// Radix words of `key` into `out` (cleared by the caller). The sort (`sort_run`) takes them a byte at a time,
    /// high byte first; the default gives none and the sort runs on `compare` alone. Format, after PR
    /// CUBRID/cubrid#8209 `btree_sort_radix.cpp` (`radix_sort_resolve_key`, `fixed_word_to_order`, `value_word`,
    /// `byte_string_word`, `resolve_string_collation`, `set_nulls_aside`):
    ///
    /// - Contract. words(a) < words(b) must imply a < b; where the words tie or one runs out, `compare` decides,
    ///   so a prefix of the words is always safe. Word i means the same part of every key of the index: a
    ///   variable-length part takes the word count its domain fixes, or the words stop there. One part per
    ///   word, a 2 or 4 B value in the high bytes. At most 64 words. Fixed-size values in a key image are host
    ///   order (`mr_index_writeval_*`); read them as numbers first.
    /// - SHORT, INTEGER, BIGINT: sign bit flipped. FLOAT, DOUBLE (MONETARY: the amount only): sign bit flipped if
    ///   positive, every bit if negative, -0.0 as +0.0. DATE, TIME, TIMESTAMP*: unsigned as stored (zone ignored).
    ///   DATETIME*: date in the high 4 B, time in the low 4 B. OBJECT/OID: volid 16 | pageid 32 | slotid 16.
    ///   ENUM: the u16 index. One word each.
    /// - BIT(n): bytes 8 at a time, zero past the value. VARBIT(n): the same, then a word of the byte count.
    /// - CHAR(n), VARCHAR(n): only for iso88591_bin, utf8_bin, iso88591_en_cs, utf8_en_cs, iso88591_en_ci,
    ///   utf8_en_ci and binary with n × codeset bytes < 255; bytes 8 at a time over ceil(n × mult / 8) words,
    ///   zero past the value; except binary, a space is 0; the en_ci ones map 'a'..'z' to 'A'..'Z'; a VARCHAR
    ///   other than binary with trailing spaces kept ends with a word of its byte count. Other collations: none.
    /// - NUMERIC(p, s) (develop encoding): a sign byte then the magnitude right-aligned in m(p) bytes
    ///   (p 1–2: 1, 3–12: 5, 13–21: 9, 22–31: 13, 32–38: 17), the first byte's 0x80 flipped, every bit flipped
    ///   for a negative value, 8 B a word. NUMERIC without precision, JSON, sets, LOBs: none.
    /// - DESC column: every word of the column flipped (`!w`), its NULL word included.
    /// - MIDXKEY: per column a NULL word (0 NULL, 1 not) then the column's words (zeros when NULL); the words stop
    ///   at the first column that has none or would pass 64 words.
    fn radix_words(&self, _key: &[u8], _out: &mut Vec<u64>) {}
}
