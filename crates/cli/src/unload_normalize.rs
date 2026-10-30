//! The rules that make two engine lines' `unloaddb` outputs comparable: where the target line prints the same
//! content differently, the source entry is rewritten to the target's form before the set comparison.
//!
//! Input: the source version and target release line, the unload file suffix, that file's entries (`unload_files`).
//! Output: the rewritten entries and, per rule, how many entries it changed on each side — the evidence that a rule
//! touched only the difference it names. Two entries of one line get no rule at all.
//! Source: the engine change cited on each rule (release/11.4 vs develop); request in workspace#502 (from the
//! workspace#484 end-to-end run, where these five differences were the whole set-comparison diff).

use std::collections::HashMap;

use crate::unload_rules_before_11_4 as before_11_4;
use fastmig_ondisk::{SourceVersion, VersionLine};

/// Which side's entries a rule rewrites.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Source,
    Target,
    Both,
}

/// How a rule rewrites an entry.
#[derive(Clone, Copy)]
pub enum Rewrite {
    /// Replace every occurrence of the first byte string with the second.
    Replace(&'static [u8], &'static [u8]),
    /// Drop the `N` of an `N'...'` literal outside string values.
    NcharLiteral,
    /// `%id <class> <n>` → `%id <class>`, `@<n>|` → `@<class>|`: class ids are numbered per unload and the two lines
    /// number their system classes differently.
    ClassIdNames,
    /// `|file:<digits>/ces_` → `|file:ces_`: drop a numeric directory in front of an LOB locator's `ces_` part.
    LobClassDir,
    /// The pre-11.4 print forms, one per function of [`crate::unload_rules_before_11_4`].
    SpOwnerAuthid,
    IndexExpression,
    SynonymOwnerCase,
    Serial11_2,
    TriggerOwner11_2,
    AutoIncrement11_2,
    LobAbsoluteLocator,
    /// Drop ` WITH DEDUPLICATE=<level>`, any level.
    DeduplicateLevel,
}

pub struct Rule {
    pub name: &'static str,
    /// Unload file suffixes the rule reads (`schema`, `indexes`, `objects`, ...).
    pub files: &'static [&'static str],
    pub side: Side,
    pub rewrite: Rewrite,
}

/// The rules of one version pair, applied in order. A 10.2 source has none: its unload prints names without owners,
/// so its reference is the official path's target-line unload (10.2 unloaddb → target loaddb
/// `--no-user-specified-name` → target unloaddb), compared as it is (workspace#552).
pub fn rules(source: SourceVersion, target: VersionLine) -> Vec<&'static Rule> {
    match (source, target) {
        (SourceVersion::V11_4, VersionLine::Develop) => V11_4_TO_DEVELOP.iter().collect(),
        (SourceVersion::V11_3, VersionLine::Develop) => {
            V11_4_TO_DEVELOP.iter().chain(BEFORE_11_4).collect()
        }
        (SourceVersion::V11_2, VersionLine::Develop) => V11_4_TO_DEVELOP
            .iter()
            .chain(BEFORE_11_4)
            .chain(V11_2_SOURCE)
            .collect(),
        // 11.4 prints what develop does in these: the rules are the source's (workspace#552)
        (SourceVersion::V11_3, VersionLine::V11_4) => {
            BEFORE_11_4.iter().chain(BEFORE_11_4_TO_11_4).collect()
        }
        (SourceVersion::V11_2, VersionLine::V11_4) => BEFORE_11_4
            .iter()
            .chain(BEFORE_11_4_TO_11_4)
            .chain(V11_2_SOURCE)
            .chain(V11_2_TO_11_4)
            .collect(),
        // one line prints alike; a downgrade is out of scope (ADR 0003)
        _ => Vec::new(),
    }
}

// 11.2 and 11.3 print stored procedures, function/filter index text, synonym owners and absolute LOB locators their
// own way; 11.2 also its serials, auto_increment state and trigger owners (T4 baselines -> develop, workspace#529;
// -> 11.4, workspace#552; `unload_rules_before_11_4`)
const BEFORE_11_4: &[Rule] = &[
    Rule {
        name: "sp-owner-authid",
        files: &["schema"],
        side: Side::Source,
        rewrite: Rewrite::SpOwnerAuthid,
    },
    Rule {
        name: "index-expression",
        files: &["indexes"],
        side: Side::Both,
        rewrite: Rewrite::IndexExpression,
    },
    Rule {
        name: "synonym-owner-case",
        files: &["schema"],
        side: Side::Source,
        rewrite: Rewrite::SynonymOwnerCase,
    },
    Rule {
        name: "lob-absolute-locator",
        files: &["objects"],
        side: Side::Source,
        rewrite: Rewrite::LobAbsoluteLocator,
    },
];

const V11_2_SOURCE: &[Rule] = &[
    Rule {
        name: "serial-11-2",
        files: &["schema"],
        side: Side::Source,
        rewrite: Rewrite::Serial11_2,
    },
    Rule {
        name: "trigger-owner-11-2",
        files: &["trigger"],
        side: Side::Source,
        rewrite: Rewrite::TriggerOwner11_2,
    },
    Rule {
        name: "auto-increment-11-2",
        files: &["schema"],
        side: Side::Target,
        rewrite: Rewrite::AutoIncrement11_2,
    },
];

// 11.2 and 11.3 lack `_db_stored_procedure_code` (1ea4a86e0 #5438), which an 11.4 target has, so user class ids shift
// by one (workspace#552)
const BEFORE_11_4_TO_11_4: &[Rule] = &[Rule {
    name: "class-id-names",
    files: &["objects"],
    side: Side::Both,
    rewrite: Rewrite::ClassIdNames,
}];

// 11.4 prints `WITH DEDUPLICATE=0` on every index and FK it did not deduplicate; 11.2 has no DEDUPLICATE indexes (R16
// workspace#532), so the target's level 0 is the only level there (workspace#552)
const V11_2_TO_11_4: &[Rule] = &[Rule {
    name: "with-deduplicate",
    files: &["schema", "indexes"],
    side: Side::Target,
    rewrite: Rewrite::DeduplicateLevel,
}];

const V11_4_TO_DEVELOP: &[Rule] = &[
    // CBRD-27324 (develop 5de66400a, CUBRID/cubrid#8104) removed the DEDUPLICATE index feature; 11.4 prints level 0,
    // or the index's level, which develop rebuilds as a plain index (R16 workspace#532, workspace#537)
    Rule {
        name: "with-deduplicate",
        files: &["schema", "indexes"],
        side: Side::Source,
        rewrite: Rewrite::DeduplicateLevel,
    },
    // CBRD-24156 (develop bb8f3cbda, CUBRID/cubrid#6420): NCHAR/VARNCHAR are CHAR/VARCHAR aliases in develop
    Rule {
        name: "national-character",
        files: &["schema"],
        side: Side::Source,
        rewrite: Rewrite::Replace(b"national character", b"character"),
    },
    // CBRD-24156, as above: the values of a former NCHAR column print as plain string literals
    Rule {
        name: "nchar-literal",
        files: &["objects"],
        side: Side::Source,
        rewrite: Rewrite::NcharLiteral,
    },
    // CBRD-25862 (develop 8ba543178, CUBRID/cubrid#6903): CT_USER_NAME "db_user" -> "_db_user"
    Rule {
        name: "db-user-class",
        files: &["schema"],
        side: Side::Source,
        rewrite: Rewrite::Replace(b"on class [db_user]", b"on class [_db_user]"),
    },
    // develop has more system classes than 11.4 (R4: 29 -> 30 tables, 20 -> 25 views), so user class ids shift
    Rule {
        name: "class-id-names",
        files: &["objects"],
        side: Side::Both,
        rewrite: Rewrite::ClassIdNames,
    },
    // CBRD-23700 (develop a59899038, CUBRID/cubrid#6833): develop keeps a class's LOB files under a directory named by
    // its HFID and attribute id, so its locators read `file:<digits>/ces_NNN/<file>`; 11.4 wrote `file:ces_NNN/<file>`.
    // Only the path notation is matched here: the `ces_NNN/<file>` part is still compared, and whether the files
    // hold the same bytes is the e2e lob-contents step's check, not this one's.
    Rule {
        name: "lob-class-dir",
        files: &["objects"],
        side: Side::Target,
        rewrite: Rewrite::LobClassDir,
    },
];

/// Applies `rules` to one file's entries of one side; returns how many entries each rule changed.
pub fn normalize(
    rules: &[&Rule],
    suffix: &str,
    source_side: bool,
    entries: &mut [Vec<u8>],
) -> Vec<usize> {
    rules
        .iter()
        .map(|r| {
            let applies = r.files.contains(&suffix)
                && match r.side {
                    Side::Source => source_side,
                    Side::Target => !source_side,
                    Side::Both => true,
                };
            if !applies {
                return 0;
            }
            match r.rewrite {
                Rewrite::Replace(from, to) => entries
                    .iter_mut()
                    .filter_map(|e| replace(e, from, to))
                    .count(),
                Rewrite::NcharLiteral => entries.iter_mut().filter_map(drop_nchar_prefix).count(),
                Rewrite::ClassIdNames => class_id_names(entries),
                Rewrite::LobClassDir => entries.iter_mut().filter_map(drop_lob_class_dir).count(),
                Rewrite::SpOwnerAuthid => before_11_4::sp_owner_authid(entries),
                Rewrite::IndexExpression => before_11_4::index_expression(entries),
                Rewrite::SynonymOwnerCase => before_11_4::synonym_owner_case(entries),
                Rewrite::Serial11_2 => before_11_4::serial_11_2(entries),
                Rewrite::TriggerOwner11_2 => before_11_4::trigger_owner_11_2(entries),
                Rewrite::AutoIncrement11_2 => before_11_4::auto_increment_11_2(entries),
                Rewrite::LobAbsoluteLocator => before_11_4::lob_absolute_locator(entries),
                Rewrite::DeduplicateLevel => entries
                    .iter_mut()
                    .filter_map(drop_deduplicate_level)
                    .count(),
            }
        })
        .collect()
}

/// `Some(())` when the entry changed.
fn replace(e: &mut Vec<u8>, from: &[u8], to: &[u8]) -> Option<()> {
    if !e.windows(from.len()).any(|w| w == from) {
        return None;
    }
    let mut out = Vec::with_capacity(e.len());
    let mut i = 0;
    while i < e.len() {
        if e[i..].starts_with(from) {
            out.extend_from_slice(to);
            i += from.len();
        } else {
            out.push(e[i]);
            i += 1;
        }
    }
    *e = out;
    Some(())
}

/// Calls `f(position, byte)` for every byte outside a quoted string value ('' is a quote inside one).
fn outside_strings(e: &[u8], mut f: impl FnMut(usize, u8)) {
    let mut in_string = false;
    for (i, &b) in e.iter().enumerate() {
        if b == b'\'' {
            in_string = !in_string;
        } else if !in_string {
            f(i, b);
        }
    }
}

/// Drops `N` where it opens a string literal: an `N` outside strings, at a token start, followed by a quote.
fn drop_nchar_prefix(e: &mut Vec<u8>) -> Option<()> {
    let mut drop = Vec::new();
    outside_strings(e, |i, b| {
        let token_start = i == 0 || matches!(e[i - 1], b' ' | b'\t' | b'\n' | b'{' | b'(' | b',');
        if b == b'N' && token_start && e.get(i + 1) == Some(&b'\'') {
            drop.push(i);
        }
    });
    if drop.is_empty() {
        return None;
    }
    for i in drop.into_iter().rev() {
        e.remove(i);
    }
    Some(())
}

/// Rewrites class ids to class names in one `_objects` file; returns the number of entries changed.
fn class_id_names(entries: &mut [Vec<u8>]) -> usize {
    // `%id <class> <n>`: the class is everything between "%id " and the last space
    let mut names: HashMap<Vec<u8>, Vec<u8>> = HashMap::new();
    for e in entries.iter() {
        if let Some(rest) = e.strip_prefix(b"%id ")
            && let Some(sp) = rest.iter().rposition(|&b| b == b' ')
        {
            names.insert(rest[sp + 1..].to_vec(), rest[..sp].to_vec());
        }
    }
    let mut changed = 0;
    for e in entries.iter_mut() {
        if let Some(rest) = e.strip_prefix(b"%id ") {
            if let Some(sp) = rest.iter().rposition(|&b| b == b' ') {
                let class = rest[..sp].to_vec();
                *e = [b"%id ".as_slice(), &class].concat();
                changed += 1;
            }
            continue;
        }
        // object references `@<n>|<instance>` outside string values
        let mut refs = Vec::new();
        outside_strings(e, |i, b| {
            if b == b'@' {
                let digits = e[i + 1..].iter().take_while(|d| d.is_ascii_digit()).count();
                if digits > 0 && e.get(i + 1 + digits) == Some(&b'|') {
                    refs.push((i + 1, i + 1 + digits));
                }
            }
        });
        if refs.is_empty() {
            continue;
        }
        let mut out = Vec::with_capacity(e.len());
        let mut at = 0;
        for (from, to) in refs {
            out.extend_from_slice(&e[at..from]);
            match names.get(&e[from..to]) {
                Some(name) => out.extend_from_slice(name),
                None => out.extend_from_slice(&e[from..to]),
            }
            at = to;
        }
        out.extend_from_slice(&e[at..]);
        *e = out;
        changed += 1;
    }
    changed
}

/// Drops `<digits>/` where it follows `|file:` and precedes `ces_` (see rule lob-class-dir).
fn drop_lob_class_dir(e: &mut Vec<u8>) -> Option<()> {
    const HEAD: &[u8] = b"|file:";
    let mut cuts = Vec::new();
    let mut i = 0;
    while let Some(at) = e[i..].windows(HEAD.len()).position(|w| w == HEAD) {
        let start = i + at + HEAD.len();
        let digits = e[start..].iter().take_while(|b| b.is_ascii_digit()).count();
        let after = start + digits;
        if digits > 0 && e[after..].starts_with(b"/ces_") {
            cuts.push((start, after + 1));
        }
        i = start;
    }
    if cuts.is_empty() {
        return None;
    }
    for (from, to) in cuts.into_iter().rev() {
        e.drain(from..to);
    }
    Some(())
}

/// Drops every ` WITH DEDUPLICATE=<digits>` of one entry.
fn drop_deduplicate_level(e: &mut Vec<u8>) -> Option<()> {
    const HEAD: &[u8] = b" WITH DEDUPLICATE=";
    let mut cuts = Vec::new();
    let mut i = 0;
    while let Some(at) = e[i..].windows(HEAD.len()).position(|w| w == HEAD) {
        let start = i + at;
        let digits = e[start + HEAD.len()..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
        if digits > 0 {
            cuts.push((start, start + HEAD.len() + digits));
        }
        i = start + HEAD.len();
    }
    if cuts.is_empty() {
        return None;
    }
    for (from, to) in cuts.into_iter().rev() {
        e.drain(from..to);
    }
    Some(())
}
