//! The set-comparison rules for a 10.2 source: where 10.2 `unloaddb` prints the same content as develop does in
//! another form. A 10.2 database has no user schema, so its unload names every class bare and sets the owner with a
//! `call [change_owner]` after the CREATE; develop qualifies the name with the owner wherever it prints it. Each
//! function rewrites one file's entries of one side in place and returns how many it changed; an entry folded into
//! another is emptied (set-comparison drops empty entries).
//!
//! Input: the entries of one unload file and the class owners of the source `_schema` file ([`ClassOwners`]).
//! Output: the rewritten entries and a count per rule.
//! Source: release/10.2 `unload_schema.c` against develop's, as the T4 10.2.18 baselines and their develop targets show
//! them (workspace#555). The commit behind each difference is on its function.

use std::collections::{HashMap, HashSet};

/// Class name → lower-case owner, from the source schema's `call [change_owner]('<class>', '<OWNER>') on class
/// [db_root];` entries. A partition (`<class>__p__<partition>`) has its partitioned class's owner (10.2 prints no
/// change_owner for it).
#[derive(Default)]
pub struct ClassOwners(HashMap<Vec<u8>, Vec<u8>>);

impl ClassOwners {
    pub fn from_schema(entries: &[Vec<u8>]) -> Self {
        let mut owners = HashMap::new();
        for e in entries {
            if let Some((class, owner)) = change_owner(e) {
                owners.insert(class.to_vec(), owner.to_ascii_lowercase());
            }
        }
        ClassOwners(owners)
    }

    fn owner(&self, class: &[u8]) -> Option<&[u8]> {
        if let Some(o) = self.0.get(class) {
            return Some(o);
        }
        let p = class.windows(5).position(|w| w == b"__p__")?;
        self.0.get(&class[..p]).map(Vec::as_slice)
    }
}

/// `('<class>', '<OWNER>')` of a `call [change_owner]` entry.
fn change_owner(e: &[u8]) -> Option<(&[u8], &[u8])> {
    let rest = e.strip_prefix(b"call [change_owner]('")?;
    let class_len = rest.iter().position(|&b| b == b'\'')?;
    let owner = rest[class_len..].strip_prefix(b"', '")?;
    let owner_len = owner.iter().position(|&b| b == b'\'')?;
    Some((&rest[..class_len], &owner[..owner_len]))
}

/// The bracketed name right after `head`, which opens `e` (`at_start`) or occurs anywhere in it (first occurrence), as
/// `(start of the name, end of the name)`.
fn bracketed_after(e: &[u8], head: &[u8], at_start: bool) -> Option<(usize, usize)> {
    let start = if at_start {
        e.starts_with(head).then_some(head.len())?
    } else {
        e.windows(head.len()).position(|w| w == head)? + head.len()
    };
    let len = e[start..].iter().position(|&b| b == b']')?;
    Some((start, start + len))
}

/// `[<class>]` at `(start, end)` (the name between the brackets) → `[<owner>].[<class>]`; `None` when the owner is
/// unknown.
fn qualify_at(e: &mut Vec<u8>, (start, end): (usize, usize), owners: &ClassOwners) -> Option<()> {
    let owner = owners.owner(&e[start..end])?.to_vec();
    e.splice(start..start, [owner.as_slice(), b"].["].concat());
    Some(())
}

/// Owner-qualified names (CBRD-23844 14742376f #3488 user schema; CBRD-24974 15766ee0c #4624 drops the
/// `call [change_owner]` lines): develop prints `[<owner>].[<class>]` where 10.2 prints `[<class>]` — the CREATE and
/// ALTER of a class or view, a GRANT, a foreign key's REFERENCES (`_schema`), the class of an index and the class
/// names its function or filter text qualifies its columns with, which develop does not print (`_indexes`), the
/// class of a trigger event (`_trigger`), the `%class` and `%id` lines (`_objects`); an auto_increment serial is
/// `[<owner>.<serial>]` (its class's owner). The class owner itself is compared through the CREATE entries.
pub fn owner_qualified_names(suffix: &str, entries: &mut [Vec<u8>], owners: &ClassOwners) -> usize {
    match suffix {
        "schema" => schema_names(entries, owners),
        "indexes" => index_names(entries, owners),
        "trigger" => entries
            .iter_mut()
            .filter_map(|e| {
                let at = bracketed_after(e, b" ON [", false)?;
                qualify_at(e, at, owners)
            })
            .count(),
        "objects" => entries
            .iter_mut()
            .filter_map(|e| {
                let at = bracketed_after(e, b"%class [", true)
                    .or_else(|| bracketed_after(e, b"%id [", true))?;
                qualify_at(e, at, owners)
            })
            .count(),
        _ => 0,
    }
}

fn schema_names(entries: &mut [Vec<u8>], owners: &ClassOwners) -> usize {
    const HEADS: [&[u8]; 4] = [
        b"CREATE CLASS [",
        b"CREATE VCLASS [",
        b"ALTER CLASS [",
        b"ALTER VCLASS [",
    ];
    let mut changed = 0;
    // the owner of the class whose attributes are printed; an auto_increment serial follows its attribute
    let mut class_owner: Option<Vec<u8>> = None;
    let mut serials: HashSet<Vec<u8>> = HashSet::new();
    for e in entries.iter_mut() {
        if change_owner(e).is_some() {
            e.clear();
            changed += 1;
            continue;
        }
        if let Some(at) = HEADS.iter().find_map(|h| bracketed_after(e, h, true)) {
            class_owner = owners.owner(&e[at.0..at.1]).map(<[u8]>::to_vec);
            let mut done = qualify_at(e, at, owners).is_some();
            if let Some(r) = bracketed_after(e, b" REFERENCES [", false) {
                done |= qualify_at(e, r, owners).is_some();
            }
            changed += usize::from(done);
            continue;
        }
        if e.starts_with(b"GRANT ")
            && let Some(at) = bracketed_after(e, b" ON [", false)
            && qualify_at(e, at, owners).is_some()
        {
            changed += 1;
            continue;
        }
        let serial = bracketed_after(e, b"ALTER SERIAL [", true).or_else(|| {
            bracketed_after(e, b"SELECT [", true).filter(|&(s, n)| serials.contains(&e[s..n]))
        });
        if let (Some((start, end)), Some(owner)) = (serial, class_owner.as_ref()) {
            serials.insert(e[start..end].to_vec());
            e.splice(start..start, [owner.as_slice(), b"."].concat());
            changed += 1;
        }
    }
    changed
}

fn index_names(entries: &mut [Vec<u8>], owners: &ClassOwners) -> usize {
    let mut changed = 0;
    for e in entries.iter_mut() {
        let Some((start, end)) = bracketed_after(e, b" ON [", false) else {
            continue;
        };
        let class = e[start..end].to_vec();
        // `[<class>].` and `<class>.` before a column in the key list or the filter, outside strings
        let mut out = e[..=end].to_vec();
        let mut in_string = false;
        let mut i = end + 1;
        while i < e.len() {
            let b = e[i];
            if b == b'\'' {
                in_string = !in_string;
            } else if !in_string && !is_ident(e[i - 1]) {
                let bracketed = [b"[", class.as_slice(), b"]."].concat();
                let bare = [class.as_slice(), b"."].concat();
                if let Some(n) = [bracketed, bare]
                    .iter()
                    .find(|p| e[i..].starts_with(p))
                    .map(Vec::len)
                {
                    i += n;
                    continue;
                }
            }
            out.push(b);
            i += 1;
        }
        let stripped = out != *e;
        *e = out;
        if qualify_at(e, (start, end), owners).is_some() || stripped {
            changed += 1;
        }
    }
    changed
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b']'
}

/// Views (CBRD-24129 25b56a1f8 #3285): 10.2 prints each view query twice, first `ADD QUERY` with `NA` columns (a
/// line ending in `; `) and later `CHANGE QUERY <n> <query>`; develop prints `ADD QUERY <query>` once. The first is
/// emptied and the second becomes the `ADD QUERY`.
pub fn view_query_one_pass(entries: &mut [Vec<u8>]) -> usize {
    let mut changed = 0;
    for e in entries.iter_mut() {
        if !e.starts_with(b"ALTER VCLASS [") {
            continue;
        }
        if e.ends_with(b" ; ") && e.windows(11).any(|w| w == b" ADD QUERY ") {
            e.clear();
            changed += 1;
        } else if let Some(at) = e.windows(14).position(|w| w == b" CHANGE QUERY ") {
            let digits = e[at + 14..]
                .iter()
                .take_while(|b| b.is_ascii_digit())
                .count();
            e.splice(at..at + 14 + digits, b" ADD QUERY".iter().copied());
            changed += 1;
        }
    }
    changed
}

/// Tables (CBRD-23708 4871d33aa #2385 made OID reuse the default): develop prints `DONT_REUSE_OID, ` before the
/// collation of a table without OID reuse, where 10.2 prints nothing; both print `REUSE_OID, ` for one with it.
pub fn dont_reuse_oid(entries: &mut [Vec<u8>]) -> usize {
    let mut changed = 0;
    for e in entries.iter_mut() {
        if !e.starts_with(b"CREATE CLASS [") || e.windows(11).any(|w| w == b" REUSE_OID,") {
            continue;
        }
        if let Some(at) = e.windows(9).position(|w| w == b" COLLATE ") {
            e.splice(at..at, b" DONT_REUSE_OID,".iter().copied());
            changed += 1;
        }
    }
    changed
}

/// Owner-qualified names in a view or trigger text (CBRD-23844; the target's text names a class or serial
/// `[<owner>.<name>]`, G8 workspace#517): the target side drops the owner part of a bracketed `<owner>.<name>` outside
/// strings, in a view query (`ALTER VCLASS` of `_schema`) or a trigger. The owner is not compared here; the name's own
/// CREATE entry compares it.
pub fn owner_qualified_text(suffix: &str, entries: &mut [Vec<u8>]) -> usize {
    let mut changed = 0;
    for e in entries
        .iter_mut()
        .filter(|e| suffix == "trigger" || e.starts_with(b"ALTER VCLASS ["))
    {
        let mut out = Vec::with_capacity(e.len());
        let mut in_string = false;
        let mut i = 0;
        while i < e.len() {
            let b = e[i];
            if b == b'\'' {
                in_string = !in_string;
            } else if !in_string
                && b == b'['
                && let Some(len) = e[i + 1..].iter().position(|&c| c == b']')
                && let Some(dot) = e[i + 1..i + 1 + len].iter().position(|&c| c == b'.')
            {
                out.push(b'[');
                i += 1 + dot + 1;
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
