//! Rewrites an 11.4 user class object into the develop class object that goes back into the same root-heap slot.
//!
//! Input: the source class, its classification, the class's new representation directory OID, the old → new
//! BTID map of the rebuilt indexes, and the partition-stream transcoder.
//! Output: the develop [`ClassObject`] (encode it with `class_object::encode_develop`), and the deferred
//! function/filter indexes it no longer lists.
//! Source: R3 § 5 rules B (kept class: layout byte-equal), C (re-encoded class: develop storage order, counts,
//! old representations removed, repid kept), F (BTIDs and rep_dir remapped; class OID, HFID, owner, triggers
//! kept: the owner and trigger rows stay in their slots, workspace#482 1), R3 § 2.3-2.4 (partition type, constraint
//! tail `index_type`/`options`, FK `ref_match_option`, DEDUPLICATE keys removed); workspace#482 4 (repid kept),
//! 5 (NCHAR → CHAR, VARNCHAR → VARCHAR in domains, defaults and partition values); workspace#486 (function and
//! filter indexes leave the target catalog); R7 § 2.4 (partition stream, workspace#497).

use fastmig_ondisk::class_object::{
    Attribute, ClassObject, ClassProperties, ClassType, Constraint, ConstraintKind, SchemaDomain,
    class_properties, format_btid, parse_btid,
};
use fastmig_ondisk::partition_stream::transcode_partition_stream;
use fastmig_ondisk::record::RecordLayout;
use fastmig_ondisk::value::{
    DbType, DecodeError, DecodeResult, Domain, OrReader, OrWriter, SetElement, SetValue, Value,
    changes_in_develop, decode_set, decode_tagged, domain_to_develop, encode_set, encode_tagged,
    value_to_develop,
};
use fastmig_ondisk::{Btid, Oid, VersionLine};

use crate::classify::ClassPlan;

const FROM: VersionLine = VersionLine::V11_4;
const TO: VersionLine = VersionLine::Develop;
/// 11.4 `DB_MAX_VARNCHAR_PRECISION` and `DB_MAX_VARCHAR_PRECISION`.
const MAX_VARNCHAR_PRECISION: i32 = 0x3fff_ffff / 2;
const MAX_VARCHAR_PRECISION: i32 = 0x3fff_ffff;
/// 11.4 `deduplicate_key.h`: `MK_DEDUPLICATE_KEY_ATTR_ID(level)` = 0x8A8B8C90 − level, level 1..14.
const DEDUP_ID_BASE: i32 = 0x8A8B_8C90_u32 as i32;

/// What the rewrite needs from the rest of the migration.
pub struct RewriteInputs<'a> {
    /// The class's new representation directory record.
    pub rep_dir: Oid,
    /// New BTID of a rebuilt index, by its old BTID; `None` writes the NULL BTID (dry run).
    pub new_btid: &'a dyn Fn(Btid) -> Option<Btid>,
}

/// A function or filter index left out of the target catalog (workspace#486).
#[derive(Clone, Debug, PartialEq)]
pub struct DeferredIndex {
    pub class: Oid,
    pub constraint: Constraint,
}

pub fn err(what: impl Into<String>) -> DecodeError {
    DecodeError {
        at: 0,
        what: what.into(),
    }
}

/// NCHAR(n) → CHAR(n), VARNCHAR(n) → VARCHAR(n) (unbounded stays unbounded); codeset and collation kept.
fn map_domain(d: &SchemaDomain) -> SchemaDomain {
    let mut o = d.clone();
    if d.type_id == DbType::Nchar as i32 {
        o.type_id = DbType::Char as i32;
    } else if d.type_id == DbType::Varnchar as i32 {
        o.type_id = DbType::Varchar as i32;
        if d.precision == MAX_VARNCHAR_PRECISION {
            o.precision = MAX_VARCHAR_PRECISION;
        }
    }
    o.set_domain = d.set_domain.iter().map(map_domain).collect();
    o
}

fn map_type_id(t: i32) -> i32 {
    match t {
        t if t == DbType::Nchar as i32 => DbType::Char as i32,
        t if t == DbType::Varnchar as i32 => DbType::Varchar as i32,
        t => t,
    }
}

/// A packed default (`or_put_value` with its domain) in develop encoding; empty stays empty.
fn map_packed_value(packed: &[u8]) -> DecodeResult<Vec<u8>> {
    if packed.is_empty() {
        return Ok(Vec::new());
    }
    let (tag, v) = decode_tagged(FROM, &mut OrReader::new(packed), Some(packed.len()))?;
    let mut w = OrWriter::new();
    encode_tagged(TO, &mut w, &domain_to_develop(&tag), &value_to_develop(&v));
    Ok(w.buf)
}

fn map_attribute(a: &Attribute) -> DecodeResult<Attribute> {
    let mut o = a.clone();
    o.type_id = map_type_id(a.type_id);
    o.domain = a.domain.iter().map(map_domain).collect();
    let ctx = |e: DecodeError| {
        err(format!(
            "attribute {}: {}",
            String::from_utf8_lossy(&a.name),
            e.what
        ))
    };
    o.default_value = map_packed_value(&a.default_value).map_err(ctx)?;
    o.original_value = map_packed_value(&a.original_value).map_err(ctx)?;
    Ok(o)
}

fn int_elem(v: i32) -> SetElement {
    SetElement {
        tag: Some(Domain::builtin(TO, DbType::Integer)),
        value: Value::Integer(v),
    }
}

fn str_elem(like: &SetElement, s: String) -> SetElement {
    SetElement {
        tag: like.tag.clone(),
        value: Value::Varchar(s.into_bytes()),
    }
}

fn map_btid_string(e: &SetElement, new_btid: &dyn Fn(Btid) -> Option<Btid>) -> SetElement {
    let old = match &e.value {
        Value::Varchar(s) | Value::Char(s) => s.clone(),
        _ => return e.clone(),
    };
    if old.starts_with(b"SHARED:") {
        return e.clone();
    }
    str_elem(e, format_btid(parse_btid(&old).and_then(new_btid)))
}

/// One constraint info sequence, 11.4 → develop. `keep` filters its key attribute ids.
fn map_constraint_info(
    kind: ConstraintKind,
    info: &SetValue,
    attr_ids: &[i32],
    new_btid: &dyn Fn(Btid) -> Option<Btid>,
) -> DecodeResult<SetValue> {
    let e = &info.elements;
    if e.len() < 3 {
        return Err(err(format!("constraint info of {} elements", e.len())));
    }
    let (body, tail) = e.split_at(e.len() - 2);
    let mut out = vec![map_btid_string(&body[0], new_btid)];
    let mut mid = &body[1..];
    let optional = match mid.last().map(|x| &x.value) {
        Some(Value::Set(_)) => {
            let o = mid.last().cloned();
            mid = &mid[..mid.len() - 1];
            o
        }
        _ => None,
    };
    for k in mid.chunks(2) {
        let id = match k[0].value {
            Value::Integer(i) => i,
            _ => return Err(err(format!("key attribute {:?}", k[0].value))),
        };
        if attr_ids.contains(&id) {
            out.extend(k.iter().cloned());
        } else if !(DEDUP_ID_BASE - 14..=DEDUP_ID_BASE - 1).contains(&id) {
            return Err(err(format!(
                "key attribute id {id} is neither an attribute nor a DEDUPLICATE key"
            )));
        }
    }
    if let Some(mut o) = optional {
        if let Value::Set(s) = &mut o.value {
            match kind {
                // FK info: PK BTID remapped, ref_match_option SM_FK_MATCH_NONE (0) appended
                ConstraintKind::ForeignKey => {
                    s.elements[1] = map_btid_string(&s.elements[1], new_btid);
                    s.elements.push(int_elem(0));
                }
                // referencing FKs: each { class, fk BTID, delete, update, name }
                ConstraintKind::PrimaryKey => {
                    for r in &mut s.elements {
                        if let Value::Set(fk) = &mut r.value
                            && fk.elements.len() > 1
                        {
                            fk.elements[1] = map_btid_string(&fk.elements[1], new_btid);
                        }
                    }
                }
                _ => {}
            }
        }
        out.push(o);
    }
    out.push(tail[0].clone()); // status
    out.push(int_elem(0)); // index_type SM_BTREE_TYPE
    out.push(int_elem(0)); // options (reserved)
    out.push(tail[1].clone()); // comment
    Ok(SetValue {
        ty: info.ty,
        domain: info.domain.clone(),
        elements: out,
    })
}

/// The class's property sequence in develop form, without the deferred indexes.
fn map_properties(
    packed: &[u8],
    props: &ClassProperties,
    attr_ids: &[i32],
    new_btid: &dyn Fn(Btid) -> Option<Btid>,
) -> DecodeResult<Vec<u8>> {
    if packed.is_empty() {
        return Ok(Vec::new());
    }
    let set = decode_set(FROM, &mut OrReader::new(packed), None)?;
    let deferred = |name: &[u8]| {
        props
            .constraints
            .iter()
            .any(|c| c.name == name && (c.has_filter || c.has_function))
    };
    let mut elements = Vec::new();
    for pair in set.elements.chunks(2) {
        let pname = match &pair[0].value {
            Value::Varchar(s) | Value::Char(s) => s.clone(),
            v => return Err(err(format!("property name {v:?}"))),
        };
        let Some(kind) = ConstraintKind::from_property(&pname) else {
            elements.extend(pair.iter().cloned());
            continue;
        };
        let Value::Set(cons) = &pair[1].value else {
            return Err(err("constraint property is not a sequence"));
        };
        let mut kept = Vec::new();
        for c in cons.elements.chunks(2) {
            let cname = match &c[0].value {
                Value::Varchar(s) | Value::Char(s) => s.clone(),
                v => return Err(err(format!("constraint name {v:?}"))),
            };
            if deferred(&cname) {
                continue; // workspace#486
            }
            let Value::Set(info) = &c[1].value else {
                return Err(err("constraint info is not a sequence"));
            };
            let info = map_constraint_info(kind, info, attr_ids, new_btid).map_err(|e| {
                err(format!(
                    "constraint {}: {}",
                    String::from_utf8_lossy(&cname),
                    e.what
                ))
            })?;
            kept.push(c[0].clone());
            kept.push(SetElement {
                tag: c[1].tag.clone(),
                value: Value::Set(info),
            });
        }
        if !kept.is_empty() {
            elements.push(pair[0].clone());
            elements.push(SetElement {
                tag: pair[1].tag.clone(),
                value: Value::Set(SetValue {
                    ty: cons.ty,
                    domain: cons.domain.clone(),
                    elements: kept,
                }),
            });
        }
    }
    let out = SetValue {
        ty: set.ty,
        domain: set.domain.clone(),
        elements,
    };
    let mut w = OrWriter::new();
    encode_set(TO, &mut w, None, &out);
    Ok(w.buf)
}

/// Partition values: bounds re-encoded; the master's expression stream (element 2) transcoded and put in a
/// VARCHAR container.
fn map_pvalues(packed: &[u8], master: bool) -> DecodeResult<Vec<u8>> {
    if packed.is_empty() {
        return Ok(Vec::new());
    }
    let (tag, v) = decode_tagged(FROM, &mut OrReader::new(packed), Some(packed.len()))?;
    let mut v = value_to_develop(&v);
    if master
        && let Value::Set(s) = &mut v
        && s.elements.len() > 2
    {
        let raw = match &s.elements[2].value {
            Value::Char(b) | Value::Varchar(b) => b.clone(),
            x => return Err(err(format!("partition stream container {x:?}"))),
        };
        // the stream is [int 0][int body_size] + body; an 11.4 CHAR container pads it with spaces
        let body = raw
            .get(4..8)
            .map_or(0, |b| i32::from_be_bytes(b.try_into().unwrap()));
        let len = 8 + usize::try_from(body).map_err(|_| err("negative stream body size"))?;
        let stream = raw
            .get(..len)
            .ok_or_else(|| err("partition stream shorter than its header"))?;
        s.elements[2] = SetElement {
            tag: Some(Domain::builtin(TO, DbType::Varchar)),
            value: Value::Varchar(transcode_partition_stream(FROM, TO, stream)?),
        };
    }
    let mut w = OrWriter::new();
    encode_tagged(TO, &mut w, &domain_to_develop(&tag), &v);
    Ok(w.buf)
}

/// The function and filter indexes of a class (workspace#486).
pub fn deferred_indexes(class: Oid, props: &ClassProperties) -> Vec<DeferredIndex> {
    props
        .constraints
        .iter()
        .filter(|c| c.has_filter || c.has_function)
        .map(|c| DeferredIndex {
            class,
            constraint: c.clone(),
        })
        .collect()
}

/// The develop class object of an 11.4 user class.
pub fn rewrite_user_class(
    src: &crate::source_classes::SourceClass,
    plan: &ClassPlan,
    inputs: &RewriteInputs,
) -> DecodeResult<ClassObject> {
    let c = &src.class;
    let mut o = c.clone();
    o.rep_dir = inputs.rep_dir;
    o.attributes = c
        .attributes
        .iter()
        .map(map_attribute)
        .collect::<DecodeResult<_>>()?;
    o.shared_attributes = c
        .shared_attributes
        .iter()
        .map(map_attribute)
        .collect::<DecodeResult<_>>()?;
    o.class_attributes = c
        .class_attributes
        .iter()
        .map(map_attribute)
        .collect::<DecodeResult<_>>()?;
    // a view has no records, but its fixed/variable split still follows develop's types
    let is_view = c.class_type == ClassType::Vclass;
    let view_changes = is_view
        && c.attributes.iter().any(|a| {
            a.domain.iter().any(|d| {
                d.to_value_domain(FROM)
                    .is_some_and(|d| changes_in_develop(&d))
            })
        });
    if plan.re_encoded || view_changes {
        // records are rewritten in the current representation, laid out as develop orders it
        let layout = RecordLayout::for_repid(FROM, c, c.repid as u32)?.to_develop();
        let mut ordered = Vec::with_capacity(o.attributes.len());
        for la in &layout.attrs {
            let a = o
                .attributes
                .iter()
                .find(|a| a.id == la.id)
                .ok_or_else(|| err(format!("layout attribute {} not in the class", la.id)))?;
            ordered.push(a.clone());
        }
        o.attributes = ordered;
        o.fixed_count = layout.n_fixed as i32;
        o.variable_count = (layout.attrs.len() - layout.n_fixed) as i32;
        o.fixed_size = layout.fixed_locations(TO)?.1 as i32;
        o.representations.clear();
    } else {
        for r in &mut o.representations {
            for a in &mut r.attributes {
                a.type_id = map_type_id(a.type_id);
                a.domain = a.domain.iter().map(map_domain).collect();
            }
        }
    }
    let props = class_properties(&c.properties, FROM)?;
    let attr_ids: Vec<i32> = c.attributes.iter().map(|a| a.id).collect();
    o.properties = map_properties(&c.properties, &props, &attr_ids, inputs.new_btid)?;
    for p in &mut o.partitions {
        p.class_partition_type = Some(if p.pname.is_none() { 1 } else { 2 });
        p.pvalues = map_pvalues(&p.pvalues, p.pname.is_none())?;
    }
    Ok(o)
}
