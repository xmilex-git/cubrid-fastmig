//! The set-comparison rules for an 11.2 or 11.3 source: where its `unloaddb` prints the same content as develop does
//! in another form. Each function rewrites one file's entries of one side in place and returns how many it changed; an
//! entry folded into another is emptied (set-comparison drops empty entries).
//!
//! Input: the entries of one unload file. Output: the rewritten entries and a count per rule.
//! Source: the print code of release/11.2 @ef544a1 (the 11.2.9 build) and release/11.3 `unload_schema.c` against
//! develop's, as the T4 baseline unloads (11.3.6, 11.2.9) and the develop targets of the same DBs show them
//! (workspace#529). The commit behind each difference is on its function.

use std::collections::HashMap;

use fastmig_catalog::sp_target_signature::split_target_signature;
use fastmig_ondisk::VersionLine;

/// The byte string between `open` and the next `close` after it, and where it ends.
fn between<'a>(e: &'a [u8], from: usize, open: &[u8], close: u8) -> Option<(&'a [u8], usize)> {
    let start = from + e[from..].windows(open.len()).position(|w| w == open)? + open.len();
    let len = e[start..].iter().position(|&b| b == close)?;
    Some((&e[start..start + len], start + len))
}

/// Empties every `call [<call>] ('<name>', '<OWNER>') ...;` entry and returns name → lower-case owner, as develop
/// qualifies the object with its owner instead of calling the owner change after the CREATE.
fn owner_calls(
    entries: &mut [Vec<u8>],
    call: &[u8],
    changed: &mut usize,
) -> HashMap<Vec<u8>, Vec<u8>> {
    let mut owner = HashMap::new();
    for e in entries.iter_mut().filter(|e| e.starts_with(call)) {
        let Some((name, end)) = between(e, 0, b"('", b'\'') else {
            continue;
        };
        let Some((user, _)) = between(e, end, b", '", b'\'') else {
            continue;
        };
        owner.insert(name.to_vec(), user.to_ascii_lowercase());
        e.clear();
        *changed += 1;
    }
    owner
}

/// `<head>[<name>]...` → `<head>[<owner>].[<name>]...` when `owner` knows the name; the bracket of the name kept.
fn qualify(e: &[u8], head: &[u8], owner: &HashMap<Vec<u8>, Vec<u8>>) -> Option<Vec<u8>> {
    let rest = e.strip_prefix(head)?;
    let name_len = rest.iter().position(|&b| b == b']')?;
    let user = owner.get(&rest[..name_len])?;
    Some([head, user.as_slice(), b"].[", rest].concat())
}

/// Stored procedures (CBRD-25184 1ea4a86e0 #5438): develop prints `CREATE FUNCTION|PROCEDURE [<owner>].[<name>]`
/// with `AUTHID OWNER` before `AS LANGUAGE` and the NAME as `target_class.target_method`; 11.2/11.3 print an
/// unqualified name, no AUTHID, the NAME as typed, and a `call [change_sp_owner]` after it.
pub fn sp_owner_authid(entries: &mut [Vec<u8>]) -> usize {
    let mut changed = 0;
    let owner = owner_calls(entries, b"call [change_sp_owner](", &mut changed);
    for e in entries.iter_mut() {
        let Some(mut out) = qualify(e, b"CREATE FUNCTION [", &owner)
            .or_else(|| qualify(e, b"CREATE PROCEDURE [", &owner))
        else {
            continue;
        };
        if replace_first(&mut out, b" AS LANGUAGE ", b" AUTHID OWNER AS LANGUAGE ").is_none() {
            continue;
        }
        if let Some((target, end)) = between(&out, 0, b" NAME '", b'\'')
            && let Some((class, method)) = split_target_signature(target, VersionLine::Develop)
        {
            let start = end - target.len();
            out = [
                &out[..start],
                class.as_slice(),
                b".",
                method.as_slice(),
                &out[end..],
            ]
            .concat();
        }
        *e = out;
        changed += 1;
    }
    changed
}

fn replace_first(e: &mut Vec<u8>, from: &[u8], to: &[u8]) -> Option<()> {
    let at = e.windows(from.len()).position(|w| w == from)?;
    e.splice(at..at + from.len(), to.iter().copied());
    Some(())
}

/// Function and filter index text: 11.2/11.3 print an index expression with owner-qualified column references
/// (`upper([app_owner.g5_fx].[name])`, `where [app_owner.g5_fx].c>10`); 11.4 and develop print the stored text
/// (`upper([name])`, `where c>10`), bracketing as the user wrote it. On both sides, for an entry whose column list
/// holds a call or that has a `where`: `[<a>.<b>].` qualifiers are dropped and so is every bracket after ` ON ` outside
/// strings, leaving only what the two forms share.
pub fn index_expression(entries: &mut [Vec<u8>]) -> usize {
    let mut changed = 0;
    for e in entries.iter_mut() {
        let Some(on) = e.windows(4).position(|w| w == b" ON ") else {
            continue;
        };
        let tail = &e[on..];
        let Some(open) = tail.iter().position(|&b| b == b'(') else {
            continue;
        };
        let has_call = tail[open + 1..].contains(&b'(');
        if !has_call && !tail.windows(7).any(|w| w == b" where ") {
            continue;
        }
        let mut out = e[..on].to_vec();
        let mut in_string = false;
        let mut i = on;
        while i < e.len() {
            let b = e[i];
            if b == b'\'' {
                in_string = !in_string;
            } else if !in_string && b == b'[' {
                // `[<a>.<b>].` is a qualifier: skip it with its dot
                if let Some(close) = e[i..].iter().position(|&c| c == b']') {
                    let inner = &e[i + 1..i + close];
                    if inner.contains(&b'.') && e.get(i + close + 1) == Some(&b'.') {
                        i += close + 2;
                        continue;
                    }
                }
                i += 1;
                continue;
            } else if !in_string && b == b']' {
                i += 1;
                continue;
            }
            out.push(b);
            i += 1;
        }
        if out != *e {
            *e = out;
            changed += 1;
        }
    }
    changed
}

/// Synonym owners: 11.2/11.3 print the owner and the target's owner as the user name (`[APP_OWNER]`), develop
/// lower-cased (`[app_owner]`, the owner-qualified name form).
pub fn synonym_owner_case(entries: &mut [Vec<u8>]) -> usize {
    let mut changed = 0;
    for e in entries.iter_mut() {
        if !(e.starts_with(b"CREATE PRIVATE SYNONYM ") || e.starts_with(b"CREATE PUBLIC SYNONYM "))
        {
            continue;
        }
        let before = e.clone();
        let mut i = 0;
        while let Some(o) = e[i..].iter().position(|&b| b == b'[') {
            let start = i + o + 1;
            let Some(len) = e[start..].iter().position(|&b| b == b']') else {
                break;
            };
            if e.get(start + len + 1) == Some(&b'.') {
                e[start..start + len].make_ascii_lowercase();
            }
            i = start + len + 1;
        }
        changed += usize::from(*e != before);
    }
    changed
}

/// 11.2's serial print (11.2.9 `unload_schema.c` emit of serials): lower-case clause lines (develop writes `noCYCLE`
/// as is), `create serial [<name>]` and `SELECT [<name>].NEXT_VALUE` with a `call [change_serial_owner]` after them
/// where develop qualifies the name, and the `call [find_user]` lines that only served that call (`to [auser]`, the
/// PUBLIC one); the find_user of a group develop prints too.
pub fn serial_11_2(entries: &mut [Vec<u8>]) -> usize {
    const CLAUSES: [(&[u8], &[u8]); 9] = [
        (b"\t increment by ", b"\t INCREMENT BY "),
        (b"\t maxvalue ", b"\t MAXVALUE "),
        (b"\t minvalue ", b"\t MINVALUE "),
        (b"\t start with ", b"\t START WITH "),
        (b"\t cache ", b"\t CACHE "),
        (b"\t nocache", b"\t NOCACHE"),
        (b"\t nocycle", b"\t noCYCLE"),
        (b"\t cycle", b"\t CYCLE"),
        (b"\t comment ", b"\t COMMENT "),
    ];
    let mut changed = 0;
    let owner = owner_calls(entries, b"call [change_serial_owner] (", &mut changed);
    for e in entries.iter_mut() {
        if e.starts_with(b"call [find_user](")
            && (e.ends_with(b" to [auser];") || e.starts_with(b"call [find_user]('PUBLIC')"))
        {
            e.clear();
            changed += 1;
        } else if let Some((from, to)) = CLAUSES.iter().find(|(f, _)| e.starts_with(f)) {
            *e = [*to, &e[from.len()..]].concat();
            changed += 1;
        } else if let Some(rest) = e.strip_prefix(b"create serial [") {
            let Some(q) = qualify(
                &[b"CREATE SERIAL [", rest].concat(),
                b"CREATE SERIAL [",
                &owner,
            ) else {
                continue;
            };
            *e = q;
            changed += 1;
        } else if let Some(q) = qualify(e, b"SELECT [", &owner) {
            *e = q;
            changed += 1;
        }
    }
    changed
}

/// Triggers: 11.2 prints `CREATE TRIGGER [<name>]` and a `call [change_trigger_owner]` after it; 11.3 and develop
/// print `CREATE TRIGGER [<owner>].[<name>]` (the trigger file).
pub fn trigger_owner_11_2(entries: &mut [Vec<u8>]) -> usize {
    let mut changed = 0;
    let owner = owner_calls(entries, b"call [change_trigger_owner](", &mut changed);
    for e in entries.iter_mut() {
        if let Some(q) = qualify(e, b"CREATE TRIGGER [", &owner) {
            *e = q;
            changed += 1;
        }
    }
    changed
}

/// An auto_increment serial's state, develop side: `ALTER SERIAL [<owner>.<name>] START WITH <current>;`, then
/// `SELECT [<owner>.<name>].NEXT_VALUE;` when it has started, then a blank entry. 11.2.9 prints one line
/// `ALTER SERIAL [<owner>].[<name>] START WITH <current + increment>;` (ef544a1 `numeric_db_value_add`; 11.2 adopted
/// develop's form later, f53128e5a CBRD-25360). The develop entries are folded into the 11.2.9 line.
/// shortcut: the increment is taken as 1 (`AUTO_INCREMENT` without a step); a serial with another step prints a
/// difference, upgrade by reading the step from the serial's CREATE when a baseline has one.
pub fn auto_increment_11_2(entries: &mut [Vec<u8>]) -> usize {
    let mut changed = 0;
    for i in 0..entries.len() {
        let Some(rest) = entries[i].strip_prefix(b"ALTER SERIAL [") else {
            continue;
        };
        let Some(name_len) = rest.iter().position(|&b| b == b']') else {
            continue;
        };
        let name = rest[..name_len].to_vec();
        let Some(dot) = name.iter().position(|&b| b == b'.') else {
            continue;
        };
        let Some((value, _)) = between(&entries[i], 0, b" START WITH ", b';') else {
            continue;
        };
        let Ok(mut current) = String::from_utf8_lossy(value).parse::<i128>() else {
            continue;
        };
        let next = [b"SELECT [", name.as_slice(), b"].NEXT_VALUE;"].concat();
        let mut j = i + 1;
        if entries.get(j) == Some(&next) {
            current += 1;
            entries[j].clear();
            changed += 1;
            j += 1;
        }
        if entries
            .get(j)
            .is_some_and(|e| e.iter().all(|b| b.is_ascii_whitespace()))
        {
            entries[j].clear();
            changed += 1;
        }
        entries[i] = format!(
            "ALTER SERIAL [{}].[{}] START WITH {current};",
            String::from_utf8_lossy(&name[..dot]),
            String::from_utf8_lossy(&name[dot + 1..])
        )
        .into_bytes();
        changed += 1;
    }
    changed
}

/// LOB locators: an 11.2/11.3 locator is absolute under the base it was written in (`|file:/<base>/ces_NNN/<file>`),
/// a develop target's relative (`|file:ces_NNN/<file>` once the rule lob-class-dir dropped its class directory). The
/// absolute directory part is dropped; the `ces_NNN/<file>` part is still compared, and the files' bytes are the e2e
/// lob-contents and lob-after-boot steps' check.
pub fn lob_absolute_locator(entries: &mut [Vec<u8>]) -> usize {
    const HEAD: &[u8] = b"|file:/";
    let mut changed = 0;
    for e in entries.iter_mut() {
        let mut out = Vec::with_capacity(e.len());
        let mut i = 0;
        while i < e.len() {
            if e[i..].starts_with(HEAD) {
                // the path runs to the next field separator or the end of the quoted value
                let path_end = e[i + 1..]
                    .iter()
                    .position(|&b| b == b'|' || b == b'\'')
                    .map_or(e.len(), |n| i + 1 + n);
                let path = &e[i + HEAD.len() - 1..path_end];
                if let Some(at) = path.windows(5).rposition(|w| w == b"/ces_") {
                    out.extend_from_slice(b"|file:");
                    out.extend_from_slice(&path[at + 1..]);
                    i = path_end;
                    continue;
                }
            }
            out.push(e[i]);
            i += 1;
        }
        if out != *e {
            *e = out;
            changed += 1;
        }
    }
    changed
}
