//! The attribute substructure of a class object: one instance, shared or class attribute.
//!
//! Input: one element of an attribute substructure set. Output: [`Attribute`].
//! Source: engine `transform_cl.c:attribute_to_disk` / `disk_to_attribute` (vars: name, value, original_value,
//! domain, triggers, properties, comment; fixed 40 B: id @0, type @4, offset @8 (0), order @12, class OID @16,
//! flags @24, index fileid @28, index pageid @32, index volid @36), `object_representation.h` ORC_ATT_*; same in
//! release/11.4 @46212adcd and develop @5b1867174; R3 § 2.2.

use super::domain::{self, SchemaDomain};
use super::or_be::{Structure, opt_string, read_oid, within};
use super::sub_set;
use crate::Oid;
use crate::value::{DecodeResult, StringCompression};

/// One attribute. Default values and properties stay in their packed form (`or_put_value`, a tagged sequence).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attribute {
    pub id: i32,
    /// Raw `DB_TYPE` of the record's release line.
    pub type_id: i32,
    /// Position in definition order (`def_order`).
    pub order: i32,
    /// Class that defines the attribute (differs from the record's class for inherited attributes).
    pub class_oid: Oid,
    /// `SM_ATTFLAG_*`.
    pub flags: i32,
    pub name: Vec<u8>,
    /// Packed current default value (`or_put_value` with domain and collation); empty for none.
    pub default_value: Vec<u8>,
    /// Packed original default value; empty for none.
    pub original_value: Vec<u8>,
    pub domain: Vec<SchemaDomain>,
    pub triggers: Vec<Oid>,
    /// Packed property sequence; empty for none.
    pub properties: Vec<u8>,
    pub comment: Option<Vec<u8>>,
}

impl Attribute {
    pub(crate) fn decode(elem: &[u8], c: StringCompression) -> DecodeResult<Attribute> {
        let s = Structure::new(elem, 7)?;
        let mut f = s.fixed();
        let id = f.i32()?;
        let type_id = f.i32()?;
        f.skip(4)?; // memory offset, always 0 on disk
        let order = f.i32()?;
        let class_oid = read_oid(&mut f)?;
        let flags = f.i32()?;
        let name = within(opt_string(s.var(0), c), "name")?.unwrap_or_default();
        let field = |e: &str| format!("attribute {}.{e}", String::from_utf8_lossy(&name));
        Ok(Attribute {
            id,
            type_id,
            order,
            class_oid,
            flags,
            default_value: s.var(1).to_vec(),
            original_value: s.var(2).to_vec(),
            domain: within(domain::list(s.var(3), c), &field("domain"))?,
            triggers: within(sub_set::oids(s.var(4)), &field("triggers"))?,
            properties: s.var(5).to_vec(),
            comment: within(opt_string(s.var(6), c), &field("comment"))?,
            name,
        })
    }
}

pub(crate) fn list(field: &[u8], c: StringCompression) -> DecodeResult<Vec<Attribute>> {
    sub_set::elements(field)?
        .into_iter()
        .map(|e| Attribute::decode(e, c))
        .collect()
}
