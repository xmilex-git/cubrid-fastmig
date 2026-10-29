//! Radix words of a develop key: u64 words whose lexicographic order never contradicts the key order, for the
//! index builder's radix sort.
//!
//! Input: a [`KeyDomain`] (develop) and a key image. Output: the words appended to a vector; none for a key the
//! format does not cover. Contract: words(a) < words(b) implies a < b; ties and prefixes are decided by
//! [`KeyDomain::compare`]. Word i means the same part of every key of the index (each column takes the word count
//! its domain fixes); at most 64 words.
//! Per column: SHORT/INTEGER/BIGINT sign bit flipped; FLOAT/DOUBLE/MONETARY amount IEEE to order (positive: sign
//! flipped, negative: all bits, -0.0 = +0.0); DATE/TIME/TIMESTAMP* unsigned; DATETIME* date high, time low;
//! OBJECT/OID volid 16 | pageid 32 | slotid 16; ENUM the index; a value of 2 or 4 bytes sits in the high bytes.
//! BIT(n): bytes 8 per word; VARBIT(n): the same, then a word of the byte count. CHAR(n)/VARCHAR(n) under
//! binary, iso88591/utf8 bin, en_cs, en_ci when n × codeset bytes < 255: bytes 8 per word, a space 0 (not under
//! binary), 'a'..'z' as 'A'..'Z' under en_ci, then for a VARCHAR other than binary a word of its byte count.
//! NUMERIC(p): a sign byte and the magnitude right-aligned in m(p) bytes, the first byte's 0x80 flipped, every bit
//! flipped for a negative value. MIDXKEY: per column a NULL word (0 NULL, 1 present) then its words (zeros for
//! NULL); words stop before a column without a format or one that would pass 64. DESC: the column's words flipped.
//! Source: CUBRID/cubrid#8209 (fec3f773a) `src/storage/btree_sort_radix.cpp` `radix_sort_resolve_key`,
//! `resolve_string_collation`, `fixed_numeric_magnitude_bytes`, `fixed_word_to_order`, `ieee_to_order`,
//! `oid_to_order`, `fixed_numeric_word`; the word layout of `fastmig_index::KeyOrder::radix_words` (workspace#493).
//! 11.4 keys get no words (only develop indexes are built).

use crate::VersionLine;
use crate::le_field::{get_u16, get_u32, get_u64};
use crate::value::db_type::DbType;
use crate::value::index_image::Collation;
use crate::value::key_domain::{KeyColumn, KeyDomain};
use crate::value::or_buf::OrReader;
use crate::value::string::codeset_mult;

/// Most words a key gets.
const MAX_WORDS: usize = 64;
/// Strings and VARBITs of this many bytes or more are stored compressed or with a long header.
const MAX_SHORT_STRING: usize = 255;

/// Engine #8209 `fixed_numeric_magnitude_bytes`: the widest magnitude the writer stores for precision `p`.
fn numeric_magnitude_bytes(p: i32) -> Option<usize> {
    match p {
        1..=2 => Some(1),
        3..=12 => Some(5),
        13..=21 => Some(9),
        22..=31 => Some(13),
        32..=38 => Some(17),
        _ => None,
    }
}

fn ieee32(bits: u32) -> u32 {
    const S: u32 = 0x8000_0000;
    if bits & S == 0 {
        bits ^ S
    } else if bits == S {
        bits
    } else {
        !bits
    }
}

fn ieee64(bits: u64) -> u64 {
    const S: u64 = 1 << 63;
    if bits & S == 0 {
        bits ^ S
    } else if bits == S {
        bits
    } else {
        !bits
    }
}

/// Bytes packed 8 per word, big end first, zero past the end, into `words` words.
fn pack(bytes: &[u8], words: usize, out: &mut Vec<u64>) {
    for w in 0..words {
        let mut b = [0u8; 8];
        let from = (w * 8).min(bytes.len());
        let to = (w * 8 + 8).min(bytes.len());
        b[..to - from].copy_from_slice(&bytes[from..to]);
        out.push(u64::from_be_bytes(b));
    }
}

/// The string shape of a CHAR/VARCHAR column: (words for the bytes, whether a length word follows).
fn string_shape(c: &KeyColumn) -> Option<(usize, bool)> {
    let coll = c.collation?;
    if !matches!(
        coll,
        Collation::Binary | Collation::ByteCs | Collation::ByteCi
    ) {
        return None;
    }
    let p = c.domain.precision;
    if p < 1 || p as usize >= MAX_SHORT_STRING {
        return None;
    }
    let max_bytes = p as usize * codeset_mult(c.domain.codeset);
    if max_bytes >= MAX_SHORT_STRING {
        return None;
    }
    // trailing spaces kept (`ignore_trailing_space` = no): of two VARCHARs equal to the shorter's end, the shorter
    // first; a CHAR drops them, binary passes over zero bytes
    let shorter_first = c.domain.ty == DbType::Varchar && coll != Collation::Binary;
    Some((max_bytes.div_ceil(8), shorter_first))
}

/// Words a column takes, or `None` when the format has none for it.
fn column_words(c: &KeyColumn) -> Option<usize> {
    use DbType as T;
    match c.domain.ty {
        T::Short | T::Integer | T::Bigint | T::Float | T::Double | T::Monetary => Some(1),
        T::Date | T::Time | T::Timestamp | T::Timestampltz | T::Timestamptz => Some(1),
        T::Datetime | T::Datetimeltz | T::Datetimetz | T::Object | T::Oid | T::Enumeration => {
            Some(1)
        }
        T::Bit if c.domain.precision >= 1 => {
            Some((c.domain.precision as usize).div_ceil(8).div_ceil(8))
        }
        T::Varbit if c.domain.precision >= 1 => {
            Some((c.domain.precision as usize).div_ceil(8).div_ceil(8) + 1)
        }
        T::Char | T::Varchar => string_shape(c).map(|(w, len)| w + len as usize),
        T::Numeric => numeric_magnitude_bytes(c.domain.precision).map(|m| (m + 1).div_ceil(8)),
        _ => None,
    }
}

/// Appends the words of one present column image; `n` is [`column_words`]. Returns false if the image does not
/// fit the shape (then the caller stops). `img` is a column of a split key (`KeyDomain::split`), so its fixed
/// fields lie inside it.
fn push_column(c: &KeyColumn, img: &[u8], n: usize, out: &mut Vec<u64>) -> bool {
    use DbType as T;
    let start = out.len();
    match c.domain.ty {
        T::Short => out.push(((get_u16(img, 0) ^ 0x8000) as u64) << 48),
        T::Integer => out.push(((get_u32(img, 0) ^ 0x8000_0000) as u64) << 32),
        T::Bigint => out.push(get_u64(img, 0) ^ (1 << 63)),
        T::Float => out.push((ieee32(get_u32(img, 0)) as u64) << 32),
        T::Double => out.push(ieee64(get_u64(img, 0))),
        T::Monetary => out.push(ieee64(get_u64(img, 4))),
        T::Date | T::Time | T::Timestamp | T::Timestampltz | T::Timestamptz => {
            out.push((get_u32(img, 0) as u64) << 32)
        }
        T::Datetime | T::Datetimeltz | T::Datetimetz => {
            out.push(((get_u32(img, 0) as u64) << 32) | get_u32(img, 4) as u64)
        }
        T::Object | T::Oid => {
            let volid = get_u16(img, 6) as u64;
            let slotid = get_u16(img, 4) as u64;
            out.push((volid << 48) | ((get_u32(img, 0) as u64) << 16) | slotid);
        }
        T::Enumeration => out.push((get_u16(img, 0) as u64) << 48),
        T::Bit => pack(img, n, out),
        T::Varbit => {
            let mut r = OrReader::new(img);
            let (h, bits) = match (r.u8(), r.i32()) {
                (Ok(0xFF), Ok(bits)) => (5, bits.max(0) as usize),
                (Ok(n), _) if n < 0xFF => (1, n as usize),
                _ => return false,
            };
            let len = bits.div_ceil(8);
            if (len).div_ceil(8) > n - 1 {
                return false;
            }
            let Some(bytes) = img.get(h..h + len) else {
                return false;
            };
            pack(bytes, n - 1, out);
            out.push(len as u64);
        }
        T::Char | T::Varchar => {
            let Some((words, len_word)) = string_shape(c) else {
                return false;
            };
            let len = img[0] as usize;
            if len >= MAX_SHORT_STRING || len > words * 8 {
                return false;
            }
            let coll = c.collation.unwrap_or(Collation::ByteCs);
            let bytes: Vec<u8> = img[1..1 + len]
                .iter()
                .map(|&b| match (coll, b) {
                    (Collation::Binary, _) => b,
                    (_, b' ') => 0,
                    (Collation::ByteCi, b'a'..=b'z') => b - (b'a' - b'A'),
                    _ => b,
                })
                .collect();
            pack(&bytes, words, out);
            if len_word {
                out.push(len as u64);
            }
        }
        T::Numeric => {
            let Some(m) = numeric_magnitude_bytes(c.domain.precision) else {
                return false;
            };
            let key_len = m + 1;
            let size = (img[0] & 0x7F) as usize;
            let negative = img[0] & 0x80 != 0;
            let Some(mag) = img.get(3..size) else {
                return false;
            };
            if mag.len() > m {
                return false;
            }
            let mut key = vec![0u8; n * 8];
            key[key_len - mag.len()..key_len].copy_from_slice(mag);
            key[0] ^= 0x80;
            if negative {
                key.iter_mut().for_each(|b| *b = !*b);
            }
            pack(&key, n, out);
        }
        _ => return false,
    }
    out.len() - start == n
}

impl KeyDomain {
    /// Appends the radix words of `key` to `out` (see the module doc). A key the format does not cover, or an 11.4
    /// key, adds no words; a MIDXKEY stops at the first column without a format.
    pub fn radix_words(&self, key: &[u8], out: &mut Vec<u64>) {
        if self.version != VersionLine::Develop {
            return;
        }
        let Ok((cols, _)) = self.split(key) else {
            return;
        };
        let base = out.len();
        for (c, img) in self.columns.iter().zip(cols) {
            let Some(n) = column_words(c) else { return };
            let need = n + self.midxkey as usize;
            if out.len() - base + need > MAX_WORDS {
                return;
            }
            let col_start = out.len();
            if self.midxkey {
                out.push(img.is_some() as u64);
            }
            match img {
                None => out.extend(std::iter::repeat_n(0, n)),
                Some(img) => {
                    if !push_column(c, img, n, out) {
                        out.truncate(col_start);
                        return;
                    }
                }
            }
            if c.domain.is_desc {
                out[col_start..].iter_mut().for_each(|w| *w = !*w);
            }
        }
    }
}
