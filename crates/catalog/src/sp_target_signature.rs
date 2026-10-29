//! Splits a pre-11.4 Java stored procedure `target` (`'Class.method(args) return type'`) into develop's
//! `target_class` and `target_method`, byte for byte as develop's own CREATE does.
//!
//! Input: the source `_db_stored_procedure.target` bytes. Output: `(target_class, target_method)`, or `None` where
//! the engine would store two empty strings (no `(`/`)`, no `.` before the `(`) — plan refuses such a source.
//! Source: engine develop @5cd1de888 `src/sp/sp_catalog.cpp:sp_split_target_signature` (:991); R12 § 4.3, § 5.1 2
//! (workspace#521). The shortening branch (method part over `SP_ATTR_TARGET_METHOD_LEN` 4096) is not ported: a
//! pre-11.4 `target` is at most 4096 bytes (boot_cl.c:3135@release/11.3), so the method part never reaches it.

/// Develop `SP_ATTR_TARGET_METHOD_LEN` (schema_system_catalog_constants.h:100).
pub const TARGET_METHOD_LEN: usize = 4096;
/// Develop `target_class` varchar(1024) (schema_system_catalog_install.cpp:931).
pub const TARGET_CLASS_LEN: usize = 1024;

/// `std::regex ("\\s+")` (ECMAScript `\s` over the ASCII bytes the engine's strings hold).
fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

pub fn split_target_signature(s: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let lparen = s.iter().rposition(|&b| b == b'(')?;
    s.iter().rposition(|&b| b == b')')?;
    let class_and_mth: Vec<u8> = s[..lparen]
        .iter()
        .copied()
        .filter(|&b| !is_space(b))
        .collect();
    let dot = class_and_mth.iter().rposition(|&b| b == b'.')?;
    let class = class_and_mth[..dot].to_vec();
    let method = [&class_and_mth[dot + 1..], &s[lparen..]].concat();
    Some((class, method))
}
