//! A class object on the same-version path (develop → develop, 11.4 → 11.4): its deferred indexes (function and
//! filter) dropped from the constraint properties, every BTID it names replaced by the rebuilt one, and its new
//! representation directory. Every other property element stays as the source wrote it: an 11.4 DEDUPLICATE index
//! keeps its hidden key in the key list, which the 11.4 B-tree builder fills (workspace#530).
//!
//! Input: the class object and its line, its new `rep_dir`, the new BTID by old BTID. Output: the class object to
//! write.
//! Source: workspace#486 (deferred indexes leave the target catalog; the same-version path too, 1st session decision
//! 2026-10-29); ADR 0005 (every index rebuilt); property layout of a constraint info sequence (`classobj_put_index`:
//! BTID string, `(attribute id, asc_desc)` pairs, optional FK info or referencing-FK list, then the tail: develop
//! status, index_type, options, comment; 11.4 status, comment, workspace#533), FK info `{class "pageid|slotid|volid",
//! PK BTID, delete, update, match}` (11.4: no match), a referencing FK `{class, FK BTID, delete, update, name}`),
//! workspace#492 resolution (BTID text `volid|fileid|root_pageid`).

use fastmig_ondisk::class_object::{
    ClassObject, ConstraintKind, class_properties, format_btid, parse_btid,
};
use fastmig_ondisk::value::{
    DecodeError, DecodeResult, OrReader, OrWriter, SetElement, SetValue, Value, decode_set,
    encode_set,
};
use fastmig_ondisk::{Btid, Oid, VersionLine};

/// Elements after the key pairs and optional: develop status, index_type, options, comment; 11.4 status, comment.
fn tail(line: VersionLine) -> usize {
    match line {
        VersionLine::V10_2 | VersionLine::V11_4 => 2,
        VersionLine::Develop => 4,
    }
}

fn err(what: String) -> DecodeError {
    DecodeError { at: 0, what }
}

fn map_btid_text(e: &mut SetElement, new_btid: &dyn Fn(Btid) -> Option<Btid>) {
    if let Value::Varchar(s) | Value::Char(s) = &mut e.value
        && !s.starts_with(b"SHARED:")
        && let Some(old) = parse_btid(s)
    {
        *s = format_btid(new_btid(old)).into_bytes();
    }
}

/// The BTIDs of one constraint info sequence of `line`.
fn map_info(
    kind: ConstraintKind,
    info: &mut SetValue,
    new_btid: &dyn Fn(Btid) -> Option<Btid>,
    line: VersionLine,
) -> DecodeResult<()> {
    let n = info.elements.len();
    let tail = tail(line);
    if n < 1 + tail {
        return Err(err(format!("constraint info of {n} elements")));
    }
    map_btid_text(&mut info.elements[0], new_btid);
    // an optional sequence sits right before the tail
    let Some(Value::Set(opt)) = info.elements.get_mut(n - tail - 1).map(|e| &mut e.value) else {
        return Ok(());
    };
    match kind {
        ConstraintKind::ForeignKey => {
            if let Some(e) = opt.elements.get_mut(1) {
                map_btid_text(e, new_btid);
            }
        }
        ConstraintKind::PrimaryKey => {
            for r in &mut opt.elements {
                if let Value::Set(fk) = &mut r.value
                    && let Some(e) = fk.elements.get_mut(1)
                {
                    map_btid_text(e, new_btid);
                }
            }
        }
        _ => {}
    }
    Ok(())
}

/// The property sequence without the deferred indexes, BTIDs replaced.
fn rewrite_properties(
    c: &ClassObject,
    line: VersionLine,
    new_btid: &dyn Fn(Btid) -> Option<Btid>,
) -> DecodeResult<Vec<u8>> {
    if c.properties.is_empty() {
        return Ok(Vec::new());
    }
    let props = class_properties(&c.properties, line)?;
    let deferred = |name: &[u8]| {
        props
            .constraints
            .iter()
            .any(|k| k.name == name && (k.has_filter || k.has_function))
    };
    let set = decode_set(line, &mut OrReader::new(&c.properties), None)?;
    let mut elements = Vec::new();
    for pair in set.elements.chunks(2) {
        let [name, value] = pair else {
            return Err(err("property sequence of odd length".into()));
        };
        let kind = match &name.value {
            Value::Varchar(s) | Value::Char(s) => ConstraintKind::from_property(s),
            v => return Err(err(format!("property name {v:?}"))),
        };
        let (Some(kind), Value::Set(cons)) = (kind, &value.value) else {
            elements.extend(pair.iter().cloned());
            continue;
        };
        let mut kept = Vec::new();
        for c in cons.elements.chunks(2) {
            let [cname, info] = c else {
                return Err(err("constraint sequence of odd length".into()));
            };
            let skip = matches!(&cname.value, Value::Varchar(s) | Value::Char(s) if deferred(s));
            if skip {
                continue; // workspace#486
            }
            let Value::Set(mut info_set) = info.value.clone() else {
                return Err(err("constraint info is not a sequence".into()));
            };
            map_info(kind, &mut info_set, new_btid, line)?;
            kept.push(cname.clone());
            kept.push(SetElement {
                tag: info.tag.clone(),
                value: Value::Set(info_set),
            });
        }
        if !kept.is_empty() {
            elements.push(name.clone());
            elements.push(SetElement {
                tag: value.tag.clone(),
                value: Value::Set(SetValue {
                    ty: cons.ty,
                    domain: cons.domain.clone(),
                    elements: kept,
                }),
            });
        }
    }
    let mut w = OrWriter::new();
    encode_set(
        line,
        &mut w,
        None,
        &SetValue {
            ty: set.ty,
            domain: set.domain.clone(),
            elements,
        },
    );
    Ok(w.buf)
}

/// `c` (of `line`) with its deferred indexes dropped, its BTIDs mapped and `rep_dir` set.
pub fn rewrite_same_version_class(
    c: &ClassObject,
    line: VersionLine,
    rep_dir: Oid,
    new_btid: &dyn Fn(Btid) -> Option<Btid>,
) -> DecodeResult<ClassObject> {
    let mut o = c.clone();
    o.properties = rewrite_properties(c, line, new_btid)?;
    o.rep_dir = rep_dir;
    Ok(o)
}
