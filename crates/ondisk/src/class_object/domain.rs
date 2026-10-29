//! The domain substructure of a class object: the declared type of an attribute, method argument or set element.
//!
//! Input: one element of a domain substructure set. Output: [`SchemaDomain`].
//! Source: engine `transform_cl.c:domain_to_disk` / `disk_to_domain2` (vars: set_domain, enumeration,
//! json_schema; fixed: type, precision, scale, codeset, collation_id, class OID = 28 B),
//! `object_representation.c:or_put_enumeration` (plain set header, then VARCHAR strings); same in
//! release/11.4 @46212adcd and develop @5b1867174. This is not the packed domain inside values (`value`).

use super::or_be::{Structure, opt_string, read_oid, within};
use super::sub_set;
use crate::value::{CollationFlag, DbType, DecodeResult, Domain, StringCompression, read_string};
use crate::{Oid, VersionLine};

/// One domain as the class object stores it. `type_id` is the raw `DB_TYPE` of the record's release line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaDomain {
    pub type_id: i32,
    pub precision: i32,
    pub scale: i32,
    pub codeset: i32,
    pub collation_id: i32,
    /// Class of an OBJECT domain; NULL otherwise.
    pub class_oid: Oid,
    /// Element domains of a set type.
    pub set_domain: Vec<SchemaDomain>,
    /// ENUM element strings, in declaration order.
    pub enumeration: Vec<Vec<u8>>,
    /// JSON schema text of a JSON domain with a schema.
    pub json_schema: Option<Vec<u8>>,
}

impl SchemaDomain {
    /// The same domain as the value codec models it (engine: the class loader builds one `TP_DOMAIN` from this
    /// substructure); `None` when the type id is not one `line` stores.
    pub fn to_value_domain(&self, line: VersionLine) -> Option<Domain> {
        Some(Domain {
            ty: DbType::from_id(line, u32::try_from(self.type_id).ok()?)?,
            precision: self.precision,
            scale: self.scale,
            codeset: self.codeset as u8,
            collation_id: self.collation_id,
            collation_flag: CollationFlag::Normal,
            is_desc: false,
            class_oid: (!self.class_oid.is_null()).then_some(self.class_oid),
            set_domain: self
                .set_domain
                .iter()
                .map(|d| d.to_value_domain(line))
                .collect::<Option<_>>()?,
            enum_labels: self.enumeration.clone(),
            json_schema: self.json_schema.clone(),
            builtin: false,
        })
    }

    pub(crate) fn decode(elem: &[u8], c: StringCompression) -> DecodeResult<SchemaDomain> {
        let s = Structure::new(elem, 3)?;
        let mut f = s.fixed();
        Ok(SchemaDomain {
            type_id: f.i32()?,
            precision: f.i32()?,
            scale: f.i32()?,
            codeset: f.i32()?,
            collation_id: f.i32()?,
            class_oid: read_oid(&mut f)?,
            set_domain: within(list(s.var(0), c), "set_domain")?,
            enumeration: within(enumeration(s.var(1), c), "enumeration")?,
            json_schema: within(opt_string(s.var(2), c), "json_schema")?,
        })
    }
}

/// A domain substructure set (an attribute's domain is a one-element list; a set type nests).
pub(crate) fn list(field: &[u8], c: StringCompression) -> DecodeResult<Vec<SchemaDomain>> {
    sub_set::elements(field)?
        .into_iter()
        .map(|e| SchemaDomain::decode(e, c))
        .collect()
}

fn enumeration(field: &[u8], c: StringCompression) -> DecodeResult<Vec<Vec<u8>>> {
    if field.is_empty() {
        return Ok(Vec::new());
    }
    let (count, mut r) = sub_set::skip_header(field)?;
    (0..count).map(|_| read_string(&mut r, c)).collect()
}
