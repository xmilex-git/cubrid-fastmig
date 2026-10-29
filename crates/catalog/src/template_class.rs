//! Instantiates a system class object of the catalog template for one target database: the template database's
//! OIDs replaced by the target's.
//!
//! Input: a template class object (develop), the template → target class OID map, and the target's heap,
//! representation directory, owner and rebuilt BTIDs. Output: the develop class object for the target slot.
//! Source: ADR 0011 (only class OIDs and owner OIDs are remapped), workspace#482 decisions 1-2, R3 § 5 rule F
//! (rep_dir, BTIDs and owner are per database; attribute `class_oid` and domain class OIDs name classes).

use fastmig_ondisk::class_object::{ClassObject, SchemaDomain, format_btid, parse_btid};
use fastmig_ondisk::value::{
    DecodeResult, OrReader, OrWriter, SetValue, Value, decode_set, encode_set,
};
use fastmig_ondisk::{Btid, Hfid, Oid, VersionLine};

/// What a template class becomes in the target.
pub struct Instance<'a> {
    /// Template class OID → target class OID; the template's own class OIDs only.
    pub class_oid: &'a dyn Fn(Oid) -> Oid,
    pub heap: Hfid,
    pub rep_dir: Oid,
    /// Template user row OID → target user row OID (DBA owns the catalog, INFORMATION_SCHEMA its views).
    pub owner: &'a dyn Fn(Oid) -> Oid,
    /// New BTID by the template's BTID; `None` writes the NULL BTID (dry run).
    pub new_btid: &'a dyn Fn(Btid) -> Option<Btid>,
}

fn map_domain(d: &mut SchemaDomain, f: &dyn Fn(Oid) -> Oid) {
    if !d.class_oid.is_null() {
        d.class_oid = f(d.class_oid);
    }
    d.set_domain.iter_mut().for_each(|s| map_domain(s, f));
}

/// Rewrites every BTID string (`volid|fileid|root`) at the constraint-info level.
fn map_btids(v: &mut Value, new_btid: &dyn Fn(Btid) -> Option<Btid>) {
    let Value::Set(s) = v else { return };
    for e in &mut s.elements {
        match &mut e.value {
            Value::Varchar(b) => {
                if let Some(old) = parse_btid(b) {
                    *b = format_btid(new_btid(old)).into_bytes();
                }
            }
            inner @ Value::Set(_) => map_btids(inner, new_btid),
            _ => {}
        }
    }
}

fn map_properties(packed: &[u8], new_btid: &dyn Fn(Btid) -> Option<Btid>) -> DecodeResult<Vec<u8>> {
    if packed.is_empty() {
        return Ok(Vec::new());
    }
    let set: SetValue = decode_set(VersionLine::Develop, &mut OrReader::new(packed), None)?;
    let mut v = Value::Set(set);
    map_btids(&mut v, new_btid);
    let Value::Set(set) = v else { unreachable!() };
    let mut w = OrWriter::new();
    encode_set(VersionLine::Develop, &mut w, None, &set);
    Ok(w.buf)
}

pub fn instantiate(t: &ClassObject, i: &Instance) -> DecodeResult<ClassObject> {
    let mut o = t.clone();
    o.heap = i.heap;
    o.rep_dir = i.rep_dir;
    o.owner = (i.owner)(t.owner);
    for a in o
        .attributes
        .iter_mut()
        .chain(o.shared_attributes.iter_mut())
        .chain(o.class_attributes.iter_mut())
    {
        a.class_oid = (i.class_oid)(a.class_oid);
        a.domain.iter_mut().for_each(|d| map_domain(d, i.class_oid));
    }
    for r in &mut o.representations {
        for a in &mut r.attributes {
            a.domain.iter_mut().for_each(|d| map_domain(d, i.class_oid));
        }
    }
    for m in o.methods.iter_mut().chain(o.class_methods.iter_mut()) {
        m.class_oid = (i.class_oid)(m.class_oid);
    }
    for f in &mut o.method_files {
        f.class_oid = (i.class_oid)(f.class_oid);
    }
    o.properties = map_properties(&t.properties, i.new_btid)?;
    Ok(o)
}
