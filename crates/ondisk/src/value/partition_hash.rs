//! The HASH partition a string key goes to, as a given release line's engine computes it: the hash value of the
//! key with its trailing ASCII spaces cut, modulo the partition count.
//!
//! Input: the release line, the key's collation id, its bytes and the partition count. Output: the partition
//! number, or `None` for a collation whose hash is the same on every line (its partitions never move).
//! Lines differ in one weight only: the space weighs 0 in 10.2's `coll.weights` and 32 from 11.0 on (11.0 moved the
//! 0 to `weights_ti`), so a key with a space inside hashes differently (R14 § 6, workspace#524).
//! Source: engine `memory_hash.c:mht_get_hash_number` (strings: trailing 0x20 cut, a NUL stops the cut, an empty or
//! all-space key is 0, then `mht2str % ht_size`; release/10.2 :2096-2131, develop :2481-2516), `language_support.c`
//! `lang_mht2str_byte` (a byte's weight), `lang_mht2str_utf8` (a code point below `w_count` by its weight, above
//! it by itself; no contractions in collations 6 and 7), `ADD_TO_HASH`, `lang_init_common_en_cs/_ci`,
//! `lang_init_coll_Utf8_tr_cs` (release/10.2 416bb667c :3120-3183, :5215-5268, :5657-5672, :5750-5830; develop
//! 5cd1de888 :3240-3303, :5387-5470, :5753-5768, :5850-5970); collation table R14 § 6.

use crate::VersionLine;
use crate::value::index_image::{TR_COUNT, tr_weights};

/// The collations whose hash depends on the space weight; the others hash raw bytes (`lang_mht2str_default`) or
/// map the space themselves (`lang_mht2str_ko`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SpaceWeighted {
    /// `lang_mht2str_byte` over EN_cs weights (the identity): utf8_bin (1), utf8_en_cs (4).
    ByteCs,
    /// `lang_mht2str_byte` over EN_ci weights ('a'..'z' as 'A'..'Z'): iso88591_en_ci (3), utf8_en_ci (5).
    ByteCi,
    /// `lang_mht2str_utf8` over the Turkish weights, `w_count` 352: utf8_tr_cs (6).
    Utf8Tr,
    /// `lang_mht2str_utf8` over EN_cs weights, `w_count` 256: utf8_ko_cs (7).
    Utf8En,
}

impl SpaceWeighted {
    fn of(collation_id: i32) -> Option<Self> {
        match collation_id {
            1 | 4 => Some(Self::ByteCs),
            3 | 5 => Some(Self::ByteCi),
            6 => Some(Self::Utf8Tr),
            7 => Some(Self::Utf8En),
            _ => None,
        }
    }
}

/// Whether HASH partitions keyed on collation `id` can sit differently under 10.2 and develop.
pub fn space_weight_changes_hash(collation_id: i32) -> bool {
    SpaceWeighted::of(collation_id).is_some()
}

fn add_to_hash(key: u32, w: u32) -> u32 {
    let k = (key << 4).wrapping_add(w);
    match k & 0xf000_0000 {
        0 => k,
        i => k ^ (i >> 24) ^ i,
    }
}

/// Engine `intl_utf8_to_cp` on a well-formed or malformed sequence: the code point and its byte length.
fn utf8_cp(s: &[u8]) -> (u32, usize) {
    let b = s[0] as u32;
    let (n, init) = match s[0] {
        0x00..=0x7F => return (b, 1),
        0xC0..=0xDF => (2, b & 0x1F),
        0xE0..=0xEF => (3, b & 0x0F),
        0xF0..=0xF7 => (4, b & 0x07),
        _ => return (b, 1),
    };
    match s.get(1..n) {
        Some(tail) => (
            tail.iter()
                .fold(init, |cp, c| (cp << 6) | (*c as u32 & 0x3F)),
            n,
        ),
        None => (b, 1),
    }
}

/// The partition number `line`'s engine gives key `s` of collation `collation_id` among `count` HASH partitions;
/// `None` when the collation hashes alike on every line or `count` is not positive.
pub fn hash_partition(line: VersionLine, collation_id: i32, s: &[u8], count: i32) -> Option<u32> {
    let kind = SpaceWeighted::of(collation_id)?;
    let count = u32::try_from(count).ok().filter(|c| *c > 0)?;
    // the cut stops at a NUL, which stays part of the key
    let end = s.iter().rposition(|&b| b != b' ').map_or(0, |i| i + 1);
    if s.first().is_none_or(|&b| b == 0) || end == 0 {
        return Some(0);
    }
    let space = if line == VersionLine::V10_2 { 0 } else { 32 };
    let byte_weight = |b: u8| -> u32 {
        match (b, kind) {
            (b' ', _) => space,
            (b'a'..=b'z', SpaceWeighted::ByteCi) => (b - (b'a' - b'A')) as u32,
            _ => b as u32,
        }
    };
    let key = &s[..end];
    let mut h = 0u32;
    match kind {
        SpaceWeighted::ByteCs | SpaceWeighted::ByteCi => {
            for &b in key {
                h = add_to_hash(h, byte_weight(b));
            }
        }
        SpaceWeighted::Utf8Tr | SpaceWeighted::Utf8En => {
            let mut i = 0;
            while i < key.len() {
                let (cp, n) = utf8_cp(&key[i..]);
                let w = match kind {
                    _ if cp == b' ' as u32 => space,
                    SpaceWeighted::Utf8Tr if (cp as usize) < TR_COUNT => tr_weights()[cp as usize],
                    // EN_cs weights are the identity below 256; above `w_count` the code point itself
                    _ => cp,
                };
                h = add_to_hash(h, w);
                i += n;
            }
        }
    }
    Some(h % count)
}
