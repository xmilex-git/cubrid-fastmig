//! The catalog template: system class objects and static catalog rows taken from a database the target engine
//! created, kept in this repository as a text file per target version and charset, and its text format.
//!
//! Input: a template file's text (or a [`CatalogTemplate`] to write). Output: the parsed template (or its text).
//! Source: ADR 0011 (docs/adr/0011-catalog-template-is-build-time-engine-output.md), workspace#482 decision 2.
//!
//! Format, one item per line, fields separated by one space:
//! - `meta <key> <value...>`: how the template was made (install, engine release, locale, page size, ...);
//! - `class <volid>|<pageid>|<slotid> <hex>`: a class object record and its OID in the template database;
//! - `row <class name> <volid>|<pageid>|<slotid> <hex>`: a heap record (MVCC header included) of a system class.
//!
//! OIDs are the template database's; regeneration remaps class OIDs and owner OIDs to the target's.

use std::fmt::Write as _;

use fastmig_ondisk::{Oid, VersionLine};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CatalogTemplate {
    /// `(key, value)` in file order.
    pub meta: Vec<(String, String)>,
    pub classes: Vec<TemplateRecord>,
    /// Rows, each tagged with its class name.
    pub rows: Vec<(String, TemplateRecord)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateRecord {
    pub oid: Oid,
    pub bytes: Vec<u8>,
}

impl CatalogTemplate {
    pub fn meta(&self, key: &str) -> Option<&str> {
        self.meta
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// The target line the template was made for (`meta line`, `11.4` or `develop`); `None` for any other value.
    pub fn line(&self) -> Option<VersionLine> {
        match self.meta("line")? {
            "11.4" => Some(VersionLine::V11_4),
            "develop" => Some(VersionLine::Develop),
            _ => None,
        }
    }

    pub fn rows_of<'a>(&'a self, class: &'a str) -> impl Iterator<Item = &'a TemplateRecord> + 'a {
        self.rows
            .iter()
            .filter(move |(c, _)| c == class)
            .map(|(_, r)| r)
    }

    pub fn to_text(&self) -> String {
        let mut s = String::new();
        for (k, v) in &self.meta {
            let _ = writeln!(s, "meta {k} {v}");
        }
        for c in &self.classes {
            let _ = writeln!(s, "class {} {}", fmt_oid(c.oid), hex(&c.bytes));
        }
        for (name, r) in &self.rows {
            let _ = writeln!(s, "row {name} {} {}", fmt_oid(r.oid), hex(&r.bytes));
        }
        s
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let mut t = CatalogTemplate::default();
        for (n, line) in text.lines().enumerate() {
            let bad = |what: &str| format!("template line {}: {what}", n + 1);
            let mut f = line.splitn(2, ' ');
            let (kind, rest) = (f.next().unwrap_or(""), f.next().unwrap_or(""));
            match kind {
                "meta" => {
                    let (k, v) = rest.split_once(' ').unwrap_or((rest, ""));
                    t.meta.push((k.to_string(), v.to_string()));
                }
                "class" => {
                    let (o, h) = rest
                        .split_once(' ')
                        .ok_or_else(|| bad("class needs oid and hex"))?;
                    t.classes.push(TemplateRecord {
                        oid: parse_oid(o).ok_or_else(|| bad("bad oid"))?,
                        bytes: unhex(h).ok_or_else(|| bad("bad hex"))?,
                    });
                }
                "row" => {
                    let mut f = rest.splitn(3, ' ');
                    let (c, o, h) = (f.next(), f.next(), f.next());
                    let (Some(c), Some(o), Some(h)) = (c, o, h) else {
                        return Err(bad("row needs class, oid and hex"));
                    };
                    t.rows.push((
                        c.to_string(),
                        TemplateRecord {
                            oid: parse_oid(o).ok_or_else(|| bad("bad oid"))?,
                            bytes: unhex(h).ok_or_else(|| bad("bad hex"))?,
                        },
                    ));
                }
                "" => {}
                k => return Err(bad(&format!("unknown item {k}"))),
            }
        }
        Ok(t)
    }
}

fn fmt_oid(o: Oid) -> String {
    format!("{}|{}|{}", o.volid, o.pageid, o.slotid)
}

fn parse_oid(s: &str) -> Option<Oid> {
    let mut p = s.split('|').map(str::parse::<i64>);
    let (v, pg, sl) = (p.next()?.ok()?, p.next()?.ok()?, p.next()?.ok()?);
    Some(Oid::new(v as i16, pg as i32, sl as i16))
}

fn hex(b: &[u8]) -> String {
    b.iter()
        .fold(String::with_capacity(2 * b.len()), |mut s, x| {
            let _ = write!(s, "{x:02x}");
            s
        })
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}
