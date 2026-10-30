//! Qualifies the names in a stored user text of a source written before user schema (view query spec, trigger
//! condition and action, partition expression) the way the engine's official path stores them: every reference to a
//! user class, stored procedure or serial becomes `[owner.name]`, the owner being the one user that owns that name
//! in the source (names had one namespace per kind before 11.2).
//!
//! Input: the text, its owner, and [`OwnerNames`] (every user class, procedure and serial with its owner prefix).
//! Output: [`QualifiedText`] — the rewritten text, and the procedures of other owners it calls.
//! Rules: a class is a name in a class position (`system_name_mapping::class_terms`); a class term with no alias
//! whose bare name the text also uses as a qualifier (`[t2].[b]`) keeps that name as its alias (`update [u2.t2]
//! [t2] set ..`), except after INTO, where no alias may stand. A procedure is a name followed by `(`, not after `.`.
//! A serial is the first argument of `serial_next_value`/`serial_current_value`, or `s` in `s.next_value`. System
//! classes and names no user object has stay as they are.
//! Source: user decision G8 workspace#517 (2026-10-30, correction: as the official path, 10.2 unloaddb → develop
//! `loaddb --no-user-specified-name`); the engine's stored texts in basedb/owner-qualified-names-102
//! expected/b-develop-engine-load.txt (view `from [u2.t2] [t2]`, `[u2.sp2]([t2].[b])`, trigger `[u2.t2]`,
//! `[dba.td]`, pexpr `FROM [dba.tdp]`); serials take their real owner where the engine writes `[dba.s]`
//! (workspace#547, an intended difference in docs/intended-differences.md); workspace#544.

use std::collections::HashMap;

use fastmig_ondisk::Oid;

use crate::sql_text_tokens::{Token, tokens};
use crate::system_name_mapping::class_terms;

/// An object a text can name: its owner and `lower(owner)`.
#[derive(Clone, Debug)]
pub struct Owned {
    pub owner: Oid,
    pub prefix: Vec<u8>,
}

/// Every user class, procedure and serial by ASCII-lowercase name.
#[derive(Clone, Debug, Default)]
pub struct OwnerNames {
    pub classes: HashMap<Vec<u8>, Owned>,
    pub procedures: HashMap<Vec<u8>, Owned>,
    pub serials: HashMap<Vec<u8>, Owned>,
}

#[derive(Clone, Debug, Default)]
pub struct QualifiedText {
    /// `None` when nothing was qualified.
    pub rewritten: Option<Vec<u8>>,
    /// Procedures of other owners the text calls, as `owner.name`.
    pub other_owner_procedures: Vec<String>,
}

fn key(n: &[u8]) -> Vec<u8> {
    n.to_ascii_lowercase()
}

fn bracketed(prefix: &[u8], name: &[u8]) -> Vec<u8> {
    [b"[".as_slice(), prefix, b".", name, b"]"].concat()
}

/// Qualifies `text`, owned by `owner` (see the module header).
pub fn qualify_text(text: &[u8], owner: Oid, names: &OwnerNames) -> QualifiedText {
    let ts = tokens(text);
    let mut out = QualifiedText::default();
    // (token index, replacement)
    let mut edits: Vec<(usize, Vec<u8>)> = Vec::new();
    let name = |k: usize| ts.get(k).and_then(|t| t.name(text));
    let word = |k: usize, w: &str| ts.get(k).is_some_and(|t| t.is_word(text, w));
    let punct = |k: usize, p: u8| ts.get(k).is_some_and(|t| t.is_punct(p));

    for (k, alias) in class_terms(text, &ts) {
        let Some(n) = name(k) else { continue };
        let Some(o) = names.classes.get(&key(n)) else {
            continue;
        };
        let mut r = bracketed(&o.prefix, n);
        let used_as_qualifier = ts.iter().enumerate().any(|(j, t)| {
            j != k && t.name(text).is_some_and(|m| m.eq_ignore_ascii_case(n)) && punct(j + 1, b'.')
        });
        if alias.is_none() && used_as_qualifier && !(k > 0 && word(k - 1, "into")) {
            r.extend_from_slice(b" [");
            r.extend_from_slice(n);
            r.push(b']');
        }
        edits.push((k, r));
    }
    for (k, t) in ts.iter().enumerate() {
        let Token::Ident { .. } = t else { continue };
        let after_dot = k > 0 && punct(k - 1, b'.');
        // serial_next_value([s], 1) / serial_current_value([s]): how the engine stores s.NEXT_VALUE
        let serial_at = if (word(k, "serial_next_value") || word(k, "serial_current_value"))
            && punct(k + 1, b'(')
        {
            Some(k + 2)
        } else if !after_dot
            && punct(k + 1, b'.')
            && (word(k + 2, "next_value") || word(k + 2, "current_value"))
        {
            Some(k)
        } else {
            None
        };
        if let Some(s) = serial_at {
            if let Some((n, o)) = name(s).and_then(|n| names.serials.get(&key(n)).map(|o| (n, o))) {
                edits.push((s, bracketed(&o.prefix, n)));
            }
            continue;
        }
        if after_dot || !punct(k + 1, b'(') || edits.iter().any(|(j, _)| *j == k) {
            continue;
        }
        let Some(n) = name(k) else { continue };
        if let Some(o) = names.procedures.get(&key(n)) {
            if o.owner != owner {
                out.other_owner_procedures.push(
                    String::from_utf8_lossy(&[o.prefix.as_slice(), b".", n].concat()).into_owned(),
                );
            }
            edits.push((k, bracketed(&o.prefix, n)));
        }
    }
    if edits.is_empty() {
        return out;
    }
    edits.sort_by_key(|e| e.0);
    edits.dedup_by_key(|e| e.0);
    let mut s = Vec::with_capacity(text.len() + 16 * edits.len());
    let mut at = 0;
    for (k, r) in edits {
        let Token::Ident { start, end, .. } = ts[k] else {
            unreachable!("only identifiers are edited");
        };
        s.extend_from_slice(&text[at..start]);
        s.extend_from_slice(&r);
        at = end;
    }
    s.extend_from_slice(&text[at..]);
    out.rewritten = Some(s);
    out
}
