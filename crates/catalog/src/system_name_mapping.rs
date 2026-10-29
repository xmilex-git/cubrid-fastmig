//! The system name mapping: a stored user text (view query spec, trigger condition or action) that names a source
//! system class develop renamed gets the develop name; a text the mapping cannot keep correct refuses the migration;
//! a name kept as it is because the rewrite would change its meaning warns.
//!
//! Input: the source version, the text, and whether the text's owner (the view's owner, the trigger's owner) is in
//! the DBA group. Output: [`TextJudgement`] — the rewritten text with its renames, or the refusals, plus warnings.
//! Rules: a name is replaced only where it is a class in a FROM or JOIN term (or after `CLASS`, `INTO`, `UPDATE`);
//! its alias and every column reference stay. Names map at once, so `db_trig` → `db_trigger` never chains on into
//! `_db_trigger`. A name that is also used outside a class position, other than as a declared alias, is not
//! rewritten.
//! Source: R11 workspace#519 § 2 table and § 5.1 (research/fastmig-r11-old-system-class-text 681715b); user decision
//! (G8 workspace#517, 2026-10-30) to rewrite instead of refusing; the renames are develop only (`1f1263217` #6443,
//! `8ba543178` #6903), so every source before develop maps; 11.4.6 prints a stored spec as
//! `from [db_trig] [db_trig]` (class then alias, workspace#542 probe); develop splits `_db_class.is_system_class`
//! (`catalog_class.c:1042-1045`, develop; 11.4 keeps every flag in it); workspace#542.

use fastmig_ondisk::SourceVersion;

use crate::sql_text_tokens::{Token, tokens};

/// What a source system class name becomes in develop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mapping {
    /// develop has no class of the old name, or one with other columns and rows; the renamed class keeps the old
    /// columns and is readable by everyone who could read the old one.
    Always(&'static str),
    /// The old name is a develop view with other columns or rows; the renamed table is readable only by the DBA
    /// group (`make_authorization (Au_dba_user, ..)`, develop), so only a DBA-group owner's text is rewritten.
    DbaOwner(&'static str),
}

const MAPPINGS: [(&str, Mapping); 6] = [
    ("db_trig", Mapping::Always("db_trigger")),
    ("db_password", Mapping::Always("_db_password")),
    ("db_trigger", Mapping::DbaOwner("_db_trigger")),
    ("db_user", Mapping::DbaOwner("_db_user")),
    ("db_authorization", Mapping::DbaOwner("_db_authorization")),
    ("db_serial", Mapping::DbaOwner("_db_serial")),
];

/// develop has no such class (1f1263217).
const REMOVED: [&str; 1] = ["db_authorizations"];
/// Tables whose old columns 11.4 renamed (`class_of`, `target`, `sp_name`): a text from an older source refuses.
const RENAMED_COLUMNS_BEFORE_11_4: [&str; 3] = [
    "_db_auth",
    "_db_stored_procedure",
    "_db_stored_procedure_args",
];

/// Keywords after which the next identifier names a class.
const CLASS_POSITION: [&str; 5] = ["from", "join", "class", "into", "update"];
/// Keywords that end a FROM list at their parenthesis depth (a later `,` no longer separates classes).
const FROM_LIST_END: [&str; 16] = [
    "where",
    "group",
    "order",
    "having",
    "limit",
    "union",
    "intersect",
    "difference",
    "except",
    "connect",
    "start",
    "for",
    "on",
    "using",
    "set",
    "values",
];
/// Keywords that can follow a class term where an alias would stand.
const NOT_ALIAS: [&str; 12] = [
    "inner",
    "left",
    "right",
    "outer",
    "cross",
    "natural",
    "join",
    "straight_join",
    "where",
    "on",
    "as",
    "with",
];

/// One replaced name: `from` at byte `at` of the source text became `to`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rename {
    pub from: &'static str,
    pub to: &'static str,
    pub at: usize,
}

#[derive(Clone, Debug, Default)]
pub struct TextJudgement {
    /// The text with every rename applied; `None` when nothing was renamed.
    pub rewritten: Option<Vec<u8>>,
    pub renames: Vec<Rename>,
    /// Non-empty means the migration does not start.
    pub refusals: Vec<String>,
    pub warnings: Vec<String>,
}

/// How a mapped or checked name occurs in one text.
#[derive(Default)]
struct Occurrences {
    /// Token indexes in a class position, each with whether an alias follows.
    class_terms: Vec<(usize, bool)>,
    /// Uses outside a class position that are not a declared alias (a qualifier of a class without an alias, a
    /// domain, a method target).
    unresolved: usize,
}

fn word_in(t: &Token, text: &[u8], words: &[&str]) -> bool {
    words.iter().any(|w| t.is_word(text, w))
}

/// Token indexes in a class position, each with the index of its alias token.
fn class_terms(text: &[u8], ts: &[Token]) -> Vec<(usize, Option<usize>)> {
    // per parenthesis depth: whether a `,` there still separates FROM-list classes
    let mut in_from = vec![false];
    let mut out = Vec::new();
    for (k, t) in ts.iter().enumerate() {
        if t.is_punct(b'(') {
            in_from.push(false);
        } else if t.is_punct(b')') {
            if in_from.len() > 1 {
                in_from.pop();
            }
        } else if let Some(f) = in_from.last_mut() {
            if t.is_word(text, "from") || t.is_word(text, "join") {
                *f = true;
            } else if word_in(t, text, &FROM_LIST_END) {
                *f = false;
            }
        }
        let Some(prev) = k.checked_sub(1).map(|p| &ts[p]) else {
            continue;
        };
        let class_position = word_in(prev, text, &CLASS_POSITION)
            || (prev.is_punct(b',') && in_from.last() == Some(&true));
        if !class_position
            || !matches!(t, Token::Ident { .. })
            || word_in(t, text, &["select", "only", "all"])
        {
            continue;
        }
        let alias = match ts.get(k + 1) {
            Some(a) if a.is_word(text, "as") => ts
                .get(k + 2)
                .filter(|n| matches!(n, Token::Ident { .. }))
                .map(|_| k + 2),
            Some(a @ Token::Ident { .. })
                if !word_in(a, text, &NOT_ALIAS) && !word_in(a, text, &FROM_LIST_END) =>
            {
                Some(k + 1)
            }
            _ => None,
        };
        out.push((k, alias));
    }
    out
}

fn eq(name: Option<&[u8]>, s: &str) -> bool {
    name.is_some_and(|n| n.eq_ignore_ascii_case(s.as_bytes()))
}

/// The occurrences of `name` in the text (see [`Occurrences`]).
fn occurrences(
    text: &[u8],
    ts: &[Token],
    terms: &[(usize, Option<usize>)],
    name: &str,
) -> Occurrences {
    let aliases: Vec<usize> = terms.iter().filter_map(|t| t.1).collect();
    let is_alias_name = aliases.iter().any(|&a| eq(ts[a].name(text), name));
    let mut o = Occurrences::default();
    for (k, t) in ts.iter().enumerate() {
        if !eq(t.name(text), name) {
            continue;
        }
        if let Some(&(_, alias)) = terms.iter().find(|(c, _)| *c == k) {
            o.class_terms.push((k, alias.is_some()));
            continue;
        }
        // an alias token, a column or attribute after `.`, a use of a declared alias
        let after_dot = k > 0 && ts[k - 1].is_punct(b'.');
        if aliases.contains(&k) || after_dot || is_alias_name {
            continue;
        }
        o.unresolved += 1;
    }
    o
}

/// Whether the class terms of a name can be replaced and leave every other use meaning the same.
fn rewritable(o: &Occurrences) -> bool {
    !o.class_terms.is_empty() && o.unresolved == 0
}

/// Judges one text (see the module header).
pub fn judge(version: SourceVersion, text: &[u8], owner_in_dba_group: bool) -> TextJudgement {
    let mut j = TextJudgement::default();
    if version == SourceVersion::Develop {
        return j;
    }
    let ts = tokens(text);
    let terms = class_terms(text, &ts);
    let used = |name: &str| {
        let o = occurrences(text, &ts, &terms, name);
        !o.class_terms.is_empty() || o.unresolved > 0
    };
    let any_token = |name: &str| ts.iter().any(|t| eq(t.name(text), name));
    let before_11_4 = matches!(
        version,
        SourceVersion::V10_2 | SourceVersion::V11_0 | SourceVersion::V11_2 | SourceVersion::V11_3
    );

    for name in REMOVED {
        if used(name) {
            j.refusals.push(format!(
                "names system class {name}, which develop no longer has (workspace#519)"
            ));
        }
    }
    if before_11_4 {
        for name in RENAMED_COLUMNS_BEFORE_11_4 {
            if used(name) {
                j.refusals.push(format!(
                    "names system class {name}, whose columns 11.4 renamed; a token pass cannot tell which it reads \
                     (workspace#519)"
                ));
            }
        }
        if used("db_serial") && any_token("att_name") {
            j.refusals.push(
                "reads db_serial.att_name, which 11.4 renamed to attr_name (workspace#519)".into(),
            );
        }
        if used("db_auth") {
            j.warnings.push("names system view db_auth, whose class_name 11.4 split into object_type and object_name".into());
        }
    }
    if (used("db_user") || used("db_root")) && any_token("triggers") {
        j.refusals.push(
            "reads triggers of db_user or db_root, a column develop dropped (workspace#519)".into(),
        );
    }
    if used("_db_class") && any_token("is_system_class") {
        j.warnings.push(
            "reads _db_class.is_system_class, which develop narrows to the system bit (other flags move to flags)"
                .into(),
        );
    }

    let mut renames = Vec::new();
    for (from, mapping) in MAPPINGS {
        let o = occurrences(text, &ts, &terms, from);
        if o.class_terms.is_empty() && o.unresolved == 0 {
            continue;
        }
        match mapping {
            Mapping::Always(to) if rewritable(&o) => renames.extend(o.class_terms.iter().map(|&(k, _)| (k, from, to))),
            Mapping::Always(to) => j.refusals.push(format!(
                "names system class {from} outside a FROM term it could be renamed in; develop names it {to} \
                 (workspace#542)"
            )),
            Mapping::DbaOwner(to) if rewritable(&o) && owner_in_dba_group => {
                renames.extend(o.class_terms.iter().map(|&(k, _)| (k, from, to)))
            }
            Mapping::DbaOwner(to) => j.warnings.push(format!(
                "names system class {from}, kept: {}; develop reads {from} as a view with other columns or rows \
                 (renamed table {to})",
                if owner_in_dba_group {
                    "it is used outside a FROM term"
                } else {
                    "its owner is not in the DBA group"
                }
            )),
        }
    }
    if renames.is_empty() || !j.refusals.is_empty() {
        return j;
    }
    renames.sort_by_key(|r| r.0);
    let mut out = Vec::with_capacity(text.len() + 8 * renames.len());
    let mut at = 0;
    for &(k, from, to) in &renames {
        let Token::Ident {
            name_start,
            name_end,
            ..
        } = ts[k]
        else {
            unreachable!("a class term is an identifier");
        };
        out.extend_from_slice(&text[at..name_start]);
        out.extend_from_slice(to.as_bytes());
        at = name_end;
        j.renames.push(Rename {
            from,
            to,
            at: name_start,
        });
    }
    out.extend_from_slice(&text[at..]);
    j.rewritten = Some(out);
    j
}
