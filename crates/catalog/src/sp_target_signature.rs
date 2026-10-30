//! Splits a pre-11.4 Java stored procedure `target` (`'Class.method(args) return type'`) into the target's
//! `target_class` and `target_method`, byte for byte as the target engine's own CREATE does, and says when the target
//! would refuse the result.
//!
//! Input: the source `_db_stored_procedure.target` bytes and the target line. Output: `(target_class,
//! target_method)`, or `None` where the engine would store two empty strings (no `(`/`)`, no `.` before the `(`);
//! [`target_refusal`] names every split the target cannot store — plan refuses such a source.
//! Source: engine `src/sp/sp_catalog.cpp:sp_split_target_signature`, the same code in release/11.4 (:971) and develop
//! @682b080b2 except `SP_ATTR_TARGET_METHOD_LEN`; R12 § 4.3, § 5.1 2 (workspace#521). The shortening branch
//! (method part over the target's `target_method` length) matters for an 11.4 target only: a pre-11.4 `target` is at
//! most 4096 bytes (boot_cl.c:3135@release/11.3). T15 workspace#538 V3: 11.4 `loaddb` stores the shortened method
//! when it fits and stops the schema load with "Data overflow" when it does not (workspace#534).

use fastmig_ondisk::VersionLine;

/// `target_class` varchar(1024) in 11.4 and develop (schema_system_catalog_install.cpp:846@release/11.4,
/// :931@develop).
pub const TARGET_CLASS_LEN: usize = 1024;

/// `SP_ATTR_TARGET_METHOD_LEN`: 1024 in 11.4, 4096 in develop (schema_system_catalog_constants.h:95@release/11.4,
/// :100@develop).
pub fn target_method_len(line: VersionLine) -> usize {
    match line {
        VersionLine::V10_2 | VersionLine::V11_4 => 1024,
        VersionLine::Develop => 4096,
    }
}

/// The Java packages whose prefix the shortening drops from a type name.
const PACKAGES: [&[u8]; 4] = [b"java.lang.", b"java.math.", b"java.sql.", b"cubrid.sql."];

/// `std::regex ("\\s+")` (ECMAScript `\s` over the ASCII bytes the engine's strings hold).
fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

fn without_spaces(s: &[u8]) -> Vec<u8> {
    s.iter().copied().filter(|&b| !is_space(b)).collect()
}

/// The package of [`PACKAGES`] `s` starts with.
fn package_at(s: &[u8]) -> Option<usize> {
    PACKAGES.iter().find(|p| s.starts_with(p)).map(|p| p.len())
}

/// `regex_replace` of `,(pkg)\.` by `,` and then of `\((pkg)\.` by `(`: a package right after a `,` or `(` goes.
/// The two passes equal one left-to-right pass: a removed package never makes a new match.
fn drop_packages(params: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(params.len());
    let mut at = 0;
    while let Some(&b) = params.get(at) {
        out.push(b);
        at += 1;
        if matches!(b, b',' | b'(')
            && let Some(n) = package_at(&params[at..])
        {
            at += n;
        }
    }
    out
}

pub fn split_target_signature(s: &[u8], line: VersionLine) -> Option<(Vec<u8>, Vec<u8>)> {
    let lparen = s.iter().rposition(|&b| b == b'(')?;
    let rparen = s.iter().rposition(|&b| b == b')')?;
    let class_and_mth = without_spaces(&s[..lparen]);
    let dot = class_and_mth.iter().rposition(|&b| b == b'.')?;
    let (class, mth) = (&class_and_mth[..dot], &class_and_mth[dot + 1..]);
    let method = [mth, &s[lparen..]].concat();
    if method.len() <= target_method_len(line) {
        return Some((class.to_vec(), method));
    }
    // `s.substr (pos_lparen, pos_rparen + 1 - pos_lparen)`; a `)` before the `(` wraps the unsigned count
    let params = match rparen + 1 {
        end if end >= lparen => &s[lparen..end],
        _ => &s[lparen..],
    };
    let params = drop_packages(&without_spaces(params));
    let ret = s
        .get(rparen..)
        .and_then(|t| t.windows(6).position(|w| w == b"return"))
        .map(|at| {
            let r = without_spaces(&s[rparen + at + 6..]);
            r[package_at(&r).unwrap_or(0)..].to_vec()
        });
    Some((
        class.to_vec(),
        [mth, &params, &ret.unwrap_or_default()].concat(),
    ))
}

/// Why a target of `line` cannot store the split of `s`; `None` when it can.
pub fn target_refusal(s: &[u8], line: VersionLine) -> Option<String> {
    let Some((class, method)) = split_target_signature(s, line) else {
        return Some("has no Class.method(..)".into());
    };
    // shortcut: lengths in bytes, the engine checks characters; a non-ASCII Java name is refused early, never
    // stored truncated
    if class.len() > TARGET_CLASS_LEN {
        return Some(format!("target class over {TARGET_CLASS_LEN} bytes"));
    }
    let max = target_method_len(line);
    (method.len() > max).then(|| format!("target method over {max} bytes after shortening"))
}
