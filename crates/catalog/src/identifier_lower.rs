//! The engine's lowercase of an identifier (`intl_identifier_lower`), which turns an owner name into the prefix of
//! an owner-qualified name: it depends on the database codeset and, for UTF-8, on the locale's identifier alphabet.
//!
//! Input: the database codeset (volume header `db_charset`), its locale name (`db_root.lang`), an identifier.
//! Output: [`IdentifierCasing`] and the lowercased bytes.
//! Source: develop `intl_support.c:intl_identifier_lower` (UTF-8 → `intl_tolower_utf8` over `ident_alphabet`,
//! ISO-8859-1 → `char_tolower_iso8859`, EUC-KR and the rest → `char_tolower`); `chartype.c` `char_lower_mapper`
//! (A-Z only) and `iso8859_lower_mapper` (also 0xC0-0xDE but 0xD7); `language_support.c` built-in UTF-8 locales:
//! `lc_English_utf8`/`lc_Korean_utf8` ident alphabet `lang_lower_EN` (A-Z only, `lang_initloc_en_utf8`),
//! `lc_Turkish_utf8` ident alphabet `lang_lower_i_TR` (A-Z, Ç Ğ Ö Ş Ü to their small letters, İ to i,
//! `lang_initloc_tr_utf8`); any other locale is loaded from its library (`lang_locale_data_load_from_lib`), whose
//! identifier alphabet is the UnicodeData one ([`crate::unicode_identifier_lower`]). The same in release/10.2
//! (workspace#544, #517).

use fastmig_ondisk::value::string::codeset;

use crate::unicode_identifier_lower;

/// How the engine lowercases an identifier in one database.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentifierCasing {
    /// A-Z only (EUC-KR, and UTF-8 `en_US`/`ko_KR`).
    Ascii,
    /// ISO-8859-1 bytes: A-Z and 0xC0-0xDE except 0xD7.
    Iso88591,
    /// UTF-8 `tr_TR`: A-Z, the Turkish capitals, İ to i.
    Turkish,
    /// UTF-8 with a locale library's alphabet: the UnicodeData lowercase of the BMP.
    LocaleLibrary,
}

/// Turkish capitals and their small letters in the identifier alphabet (`special_upper_cp`/`special_lower_cp`, İ → i).
const TURKISH: [(char, char); 6] = [
    ('\u{c7}', '\u{e7}'),
    ('\u{11e}', '\u{11f}'),
    ('\u{130}', 'i'),
    ('\u{d6}', '\u{f6}'),
    ('\u{15e}', '\u{15f}'),
    ('\u{dc}', '\u{fc}'),
];

impl IdentifierCasing {
    // shortcut: en_US, ko_KR and tr_TR are taken as built in; a target whose cubrid_locales.txt loads a library for
    // one of them lowercases non-ASCII owners by UnicodeData instead. Upgrade when such a source shows up.
    pub fn of(charset: u8, lang: &[u8]) -> Self {
        match charset {
            codeset::UTF8 => match lang {
                b"en_US" | b"ko_KR" => IdentifierCasing::Ascii,
                b"tr_TR" => IdentifierCasing::Turkish,
                _ => IdentifierCasing::LocaleLibrary,
            },
            codeset::ISO88591 => IdentifierCasing::Iso88591,
            _ => IdentifierCasing::Ascii,
        }
    }

    /// The identifier lowercased as the engine does.
    pub fn lower(self, s: &[u8]) -> Vec<u8> {
        let utf8 = |map: &dyn Fn(char) -> char| match std::str::from_utf8(s) {
            Ok(t) => t.chars().map(map).collect::<String>().into_bytes(),
            // invalid UTF-8 is copied byte by byte, like the engine with validation off
            Err(_) => s.to_ascii_lowercase(),
        };
        match self {
            IdentifierCasing::Ascii => s.to_ascii_lowercase(),
            IdentifierCasing::Iso88591 => s
                .iter()
                .map(|&b| match b {
                    b'A'..=b'Z' | 0xC0..=0xD6 | 0xD8..=0xDE => b + 0x20,
                    _ => b,
                })
                .collect(),
            IdentifierCasing::Turkish => utf8(&|c| match TURKISH.iter().find(|(u, _)| *u == c) {
                Some((_, l)) => *l,
                None => c.to_ascii_lowercase(),
            }),
            // every lowercase in the table is a valid char (it is a UnicodeData code point)
            IdentifierCasing::LocaleLibrary => {
                utf8(&|c| char::from_u32(unicode_identifier_lower::lower(c as u32)).unwrap_or(c))
            }
        }
    }
}
