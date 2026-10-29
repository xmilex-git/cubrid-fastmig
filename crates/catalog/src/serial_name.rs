//! The name develop gives an auto_increment serial, which can differ from the 11.4 name when the class and
//! attribute names are long. develop finds the serial by that name when it loads the class, so `_db_serial`
//! rows whose 11.4 name differs are renamed.
//!
//! Input: the class unique name (`owner.class`, lowercase), the attribute name (lowercase) and the DB codeset.
//! Output: develop's serial unique name.
//! Source: develop @5b1867174 `transform.c:set_auto_increment_serial_name` / `set_auto_increment_serial_partial_name`
//! (217ea8c44, PR #7230); 11.4 `transform.h` `SET_AUTO_INCREMENT_SERIAL_NAME` (always `owner.class_ai_attr`);
//! workspace#482 (consequence: renaming is required).

use fastmig_ondisk::value::string::codeset;
use md5::{Digest, Md5};

/// `DB_MAX_SERIAL_NAME_LENGTH`.
const MAX_SERIAL_NAME: usize = 223;
/// `"_ai_"`.
const EXTRA: usize = 4;
/// `'_'` + 32 hex digits.
const MD5_PART: usize = 33;

/// Bytes of the longest prefix of `s` within `max` bytes that ends on a character boundary (engine
/// `intl_identifier_fix`).
fn char_prefix(s: &[u8], max: usize, cs: u8) -> usize {
    let mut pos = 0;
    while pos < s.len() {
        let b = s[pos];
        let w = match cs {
            codeset::UTF8 => match b {
                0x00..=0x7F => 1,
                0xC0..=0xDF => 2,
                0xE0..=0xEF => 3,
                0xF0..=0xF7 => 4,
                _ => 1,
            },
            codeset::KSC5601_EUC => match b {
                0x00..=0x7F => 1,
                0x8F => 3,
                _ => 2,
            },
            _ => 1,
        };
        if pos + w > max {
            break;
        }
        pos += w;
    }
    pos
}

fn partial(name1: &[u8], copy_len: usize, name2: &[u8], cs: u8) -> Vec<u8> {
    let mut out = name1[..char_prefix(name1, copy_len, cs)].to_vec();
    out.extend_from_slice(name2);
    out
}

/// The 11.4 serial name: `owner.class_ai_attr`.
pub fn name_11_4(class: &[u8], attr: &[u8]) -> Vec<u8> {
    [class, b"_ai_", attr].concat()
}

/// develop's serial name for `class` (`owner.class`) and `attr`.
pub fn name_develop(class: &[u8], attr: &[u8], cs: u8) -> Vec<u8> {
    let dot = class.iter().position(|&b| b == b'.').unwrap_or(0);
    let class_only = &class[dot + 1..];
    if class_only.len() + EXTRA + attr.len() < MAX_SERIAL_NAME {
        return name_11_4(class, attr);
    }
    let hashed = [class_only, b"_ai_", attr].concat();
    let md5: String = Md5::digest(&hashed)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let md5_part = [b"_".as_slice(), md5.as_bytes()].concat();
    let space = MAX_SERIAL_NAME - (EXTRA + MD5_PART);
    let first = partial(class_only, space / 2, b"_ai_", cs);
    let second = partial(attr, space - first.len(), &md5_part, cs);
    [&class[..=dot], &first[..], &second[..]].concat()
}
