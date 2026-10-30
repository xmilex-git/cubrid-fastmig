//! The constraints and indexes a class object keeps in its property sequence: one entry per primary key, unique,
//! foreign key, plain index (and their reverse variants), with BTID, keys and the version-dependent tail.
//!
//! Input: a class object's packed property sequence and its release line. Output: [`ClassProperties`]: the
//! property pairs and the parsed [`Constraint`]s.
//! Source: engine `class_object.c:classobj_put_index` (develop @5b1867174 :961-1215, release/11.4 @46212adcd
//! :972-1215), `storage_common.h` `SM_PROPERTY_*` names, `SM_CONSTRAINT_*_INDEX` (counted from the end: develop
//! comment 1, options 2, index_type 3, status 4, optional 5; 11.4 comment 1, status 2, optional 3),
//! `classobj_make_foreign_key_info_seq` (ref class OID as `pageid|slotid|volid`, PK BTID as `volid|fileid|root`,
//! delete, update; develop adds `ref_match_option`, 11.4 has 4 elements), `SM_FILTER_INDEX_ID` "*FP*",
//! `SM_FUNCTION_INDEX_ID` "*FI*", `SM_PREFIX_INDEX_ID` "*PLID*"; R3 § 2.4.

use crate::value::{DecodeError, DecodeResult, OrReader, SetValue, Value, decode_set};
use crate::{Btid, Oid, VersionLine, Vfid};

/// Engine `SM_CONSTRAINT_TYPE`, by its property name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ConstraintKind {
    /// `*U`
    Unique,
    /// `*I`
    Index,
    /// `*RU`
    ReverseUnique,
    /// `*RI`
    ReverseIndex,
    /// `*P`
    PrimaryKey,
    /// `*FK`
    ForeignKey,
}

impl ConstraintKind {
    pub fn from_property(name: &[u8]) -> Option<Self> {
        Some(match name {
            b"*U" => Self::Unique,
            b"*I" => Self::Index,
            b"*RU" => Self::ReverseUnique,
            b"*RI" => Self::ReverseIndex,
            b"*P" => Self::PrimaryKey,
            b"*FK" => Self::ForeignKey,
            _ => return None,
        })
    }

    pub fn property(self) -> &'static str {
        match self {
            Self::Unique => "*U",
            Self::Index => "*I",
            Self::ReverseUnique => "*RU",
            Self::ReverseIndex => "*RI",
            Self::PrimaryKey => "*P",
            Self::ForeignKey => "*FK",
        }
    }
}

/// Foreign key info (`SM_FK_INFO_*`).
#[derive(Clone, Debug, PartialEq)]
pub struct ForeignKeyInfo {
    pub ref_class: Oid,
    /// The referenced primary key's index.
    pub ref_pk_btid: Option<Btid>,
    pub delete_action: i32,
    pub update_action: i32,
    /// develop only; 11.4 has no match option (develop default `SM_FK_MATCH_NONE` = 0).
    pub match_option: Option<i32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Constraint {
    pub kind: ConstraintKind,
    pub name: Vec<u8>,
    /// The index; `None` for a constraint sharing another's index (`SHARED:<name>`).
    pub btid: Option<Btid>,
    /// `SHARED:<name>`: the constraint whose index this one uses.
    pub shared_with: Option<Vec<u8>>,
    /// `(attribute id, asc_desc)` per key in key order; asc_desc 1 = DESC. An 11.3/11.4 DEDUPLICATE index's hidden
    /// key is not among them (see `deduplicate_key`).
    pub keys: Vec<(i32, i32)>,
    /// The hidden key of an 11.3/11.4 DEDUPLICATE index (workspace#532).
    pub deduplicate_key: Option<super::DeduplicateKey>,
    /// The optional element as stored (FK info, PK's referencing FKs, or the named filter/function/prefix
    /// sequence).
    pub optional: Option<Value>,
    pub foreign_key: Option<ForeignKeyInfo>,
    /// Has a `*FP*` filter predicate.
    pub has_filter: bool,
    /// Has a `*FI*` function key.
    pub has_function: bool,
    /// `SM_INDEX_STATUS`.
    pub status: i32,
    /// develop only.
    pub index_type: Option<i32>,
    /// develop only.
    pub options: Option<i32>,
    pub comment: Option<Vec<u8>>,
}

/// A class's property sequence: `(name, value)` pairs, constraints parsed out.
#[derive(Clone, Debug, PartialEq)]
pub struct ClassProperties {
    pub pairs: Vec<(Vec<u8>, Value)>,
    pub constraints: Vec<Constraint>,
}

fn bad(what: String) -> DecodeError {
    DecodeError { at: 0, what }
}

fn string(v: &Value) -> Option<&[u8]> {
    match v {
        Value::Varchar(s) | Value::Char(s) | Value::Varnchar(s) | Value::Nchar(s) => Some(s),
        _ => None,
    }
}

fn int(v: &Value) -> Option<i32> {
    match v {
        Value::Integer(i) => Some(*i),
        Value::Short(i) => Some(*i as i32),
        _ => None,
    }
}

/// `volid|fileid|root_pageid`, the form `classobj_put_index` writes; `None` for the NULL BTID.
pub fn parse_btid(s: &[u8]) -> Option<Btid> {
    let s = std::str::from_utf8(s).ok()?;
    let mut p = s.split('|').map(str::parse::<i32>);
    let (v, f, r) = (p.next()?.ok()?, p.next()?.ok()?, p.next()?.ok()?);
    (f != -1).then(|| Btid {
        vfid: Vfid::new(v as i16, f),
        pageid: r,
    })
}

/// `pageid|slotid|volid`, the form `classobj_make_foreign_key_info_seq` writes a referenced class OID in.
pub fn parse_oid(s: &[u8]) -> Option<Oid> {
    let s = std::str::from_utf8(s).ok()?;
    let mut p = s.split('|').map(str::parse::<i32>);
    let (pg, sl, v) = (p.next()?.ok()?, p.next()?.ok()?, p.next()?.ok()?);
    Some(Oid::new(v as i16, pg, sl as i16))
}

pub fn format_btid(b: Option<Btid>) -> String {
    match b {
        Some(b) => format!("{}|{}|{}", b.vfid.volid, b.vfid.fileid, b.pageid),
        None => "-1|-1|-1".into(),
    }
}

fn pairs(set: &SetValue) -> DecodeResult<Vec<(Vec<u8>, Value)>> {
    let e = &set.elements;
    if !e.len().is_multiple_of(2) {
        return Err(bad(format!("property sequence of odd length {}", e.len())));
    }
    e.chunks(2)
        .map(|p| {
            let name = string(&p[0].value)
                .ok_or_else(|| bad(format!("property name {:?}", p[0].value)))?;
            Ok((name.to_vec(), p[1].value.clone()))
        })
        .collect()
}

/// The optional element of an index is a sequence of `{name, info}` sequences
/// (`classobj_put_seq_with_name_and_iterate`); true when one is named `name`.
fn named(seq: &Value, name: &[u8]) -> bool {
    let Value::Set(s) = seq else { return false };
    s.elements.iter().any(|e| match &e.value {
        Value::Set(p) => p.elements.first().and_then(|n| string(&n.value)) == Some(name),
        _ => false,
    })
}

fn foreign_key(v: &Value) -> DecodeResult<ForeignKeyInfo> {
    let Value::Set(s) = v else {
        return Err(bad(format!("FK info {v:?}")));
    };
    let e: Vec<&Value> = s.elements.iter().map(|e| &e.value).collect();
    if e.len() < 4 {
        return Err(bad(format!("FK info has {} elements", e.len())));
    }
    let ref_class = string(e[0])
        .and_then(parse_oid)
        .ok_or_else(|| bad(format!("FK ref class {:?}", e[0])))?;
    Ok(ForeignKeyInfo {
        ref_class,
        ref_pk_btid: string(e[1]).and_then(parse_btid),
        delete_action: int(e[2]).ok_or_else(|| bad("FK delete action".into()))?,
        update_action: int(e[3]).ok_or_else(|| bad("FK update action".into()))?,
        match_option: e.get(4).and_then(|v| int(v)),
    })
}

fn constraint(
    kind: ConstraintKind,
    name: &[u8],
    info: &Value,
    line: VersionLine,
) -> DecodeResult<Constraint> {
    let Value::Set(s) = info else {
        return Err(bad(format!("constraint info {info:?}")));
    };
    let e: Vec<&Value> = s.elements.iter().map(|e| &e.value).collect();
    let tail = match line {
        VersionLine::V10_2 | VersionLine::V11_4 => 2,
        VersionLine::Develop => 4,
    };
    if e.len() < 1 + tail {
        return Err(bad(format!("constraint info has {} elements", e.len())));
    }
    let id = string(e[0]).ok_or_else(|| bad("constraint BTID".into()))?;
    let (btid, shared_with) = match id.strip_prefix(b"SHARED:") {
        Some(n) => (None, Some(n.to_vec())),
        None => (parse_btid(id), None),
    };
    // an 11.3/11.4 DEDUPLICATE index lists its hidden key like an attribute; `keys` holds the real ones and the hidden
    // one is kept apart, so a develop target drops it and an 11.4 target puts it back last (workspace#537, #530)
    let deduplicate_key = s.elements[1..e.len() - tail]
        .as_chunks::<2>()
        .0
        .iter()
        .find_map(|k| {
            Some(super::DeduplicateKey {
                level: int(&k[0].value).and_then(super::deduplicate_level)?,
                desc: int(&k[1].value) == Some(1),
            })
        });
    let stripped = match line {
        VersionLine::V10_2 | VersionLine::V11_4 => {
            super::without_deduplicate_key(kind, &s.elements[1..e.len() - tail])
        }
        VersionLine::Develop => s.elements[1..e.len() - tail].to_vec(),
    };
    let body: Vec<&Value> = stripped.iter().map(|e| &e.value).collect();
    let mut mid = body.as_slice();
    let optional = match mid.last() {
        Some(Value::Set(_)) => {
            let o = (*mid.last().unwrap()).clone();
            mid = &mid[..mid.len() - 1];
            Some(o)
        }
        _ => None,
    };
    if !mid.len().is_multiple_of(2) {
        return Err(bad(format!("{} key elements", mid.len())));
    }
    let keys = mid
        .chunks(2)
        .map(|k| {
            Ok((
                int(k[0]).ok_or_else(|| bad(format!("key attribute {:?}", k[0])))?,
                int(k[1]).unwrap_or(0),
            ))
        })
        .collect::<DecodeResult<_>>()?;
    let t = &e[e.len() - tail..];
    let foreign_key = match (kind, &optional) {
        (ConstraintKind::ForeignKey, Some(o)) => Some(foreign_key(o)?),
        _ => None,
    };
    let special = kind != ConstraintKind::ForeignKey && kind != ConstraintKind::PrimaryKey;
    Ok(Constraint {
        kind,
        name: name.to_vec(),
        btid,
        shared_with,
        keys,
        deduplicate_key,
        has_filter: special && optional.as_ref().is_some_and(|o| named(o, b"*FP*")),
        has_function: special && optional.as_ref().is_some_and(|o| named(o, b"*FI*")),
        optional,
        foreign_key,
        status: int(t[0]).unwrap_or(0),
        index_type: (line == VersionLine::Develop).then(|| int(t[1]).unwrap_or(0)),
        options: (line == VersionLine::Develop).then(|| int(t[2]).unwrap_or(0)),
        comment: string(t[tail - 1]).map(<[u8]>::to_vec),
    })
}

/// Parses a packed property sequence (class `properties`); empty input gives no properties.
pub fn class_properties(packed: &[u8], line: VersionLine) -> DecodeResult<ClassProperties> {
    if packed.is_empty() {
        return Ok(ClassProperties {
            pairs: Vec::new(),
            constraints: Vec::new(),
        });
    }
    let set = decode_set(line, &mut OrReader::new(packed), None)?;
    let pairs = pairs(&set)?;
    let mut constraints = Vec::new();
    for (pname, v) in &pairs {
        let Some(kind) = ConstraintKind::from_property(pname) else {
            continue;
        };
        let Value::Set(s) = v else {
            return Err(bad(format!(
                "property {} is {v:?}",
                String::from_utf8_lossy(pname)
            )));
        };
        for (cname, info) in self::pairs(s)? {
            constraints.push(constraint(kind, &cname, &info, line).map_err(|e| {
                bad(format!(
                    "constraint {}: {}",
                    String::from_utf8_lossy(&cname),
                    e.what
                ))
            })?);
        }
    }
    Ok(ClassProperties { pairs, constraints })
}
