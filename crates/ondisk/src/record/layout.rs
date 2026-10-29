//! A record layout: one representation's attributes in storage order with their domains, built from a decoded class
//! object, and where each fixed attribute sits.
//!
//! Input: a [`ClassObject`] (current representation = its attributes; older ones = `representations`) and a repid.
//! Output: [`RecordLayout`], the input of [`crate::record::record_codec`]; [`schema_domain`] turns the class
//! object's domain substructure into a value [`Domain`].
//! Source: engine `src/base/object_representation_sr.c` `or_get_current_representation`,
//! `or_get_old_representation` (fixed attributes first, `location` = running `tp_domain_disk_size`,
//! `fixed_length = DB_ATT_ALIGN`; develop @5b1867174 :2883-3073, same in release/11.4 @46212adcd); R2 § 1.7.

use crate::VersionLine;
use crate::class_object::{ClassObject, SchemaDomain};
use crate::value::db_type::DbType;
use crate::value::domain::{CollationFlag, Domain};
use crate::value::or_buf::{DecodeError, DecodeResult};

/// Converts the class object's domain (its first element; collections recurse into `set_domain`).
pub fn schema_domain(version: VersionLine, chain: &[SchemaDomain]) -> DecodeResult<Domain> {
    let s = chain.first().ok_or_else(|| DecodeError {
        at: 0,
        what: "attribute without a domain".into(),
    })?;
    let ty = DbType::from_id(version, s.type_id as u32).ok_or_else(|| DecodeError {
        at: 0,
        what: format!("domain type id {}", s.type_id),
    })?;
    Ok(Domain {
        ty,
        precision: s.precision,
        scale: s.scale,
        codeset: s.codeset as u8,
        collation_id: s.collation_id,
        collation_flag: CollationFlag::Normal,
        is_desc: false,
        class_oid: (!s.class_oid.is_null()).then_some(s.class_oid),
        set_domain: s
            .set_domain
            .iter()
            .map(|e| schema_domain(version, std::slice::from_ref(e)))
            .collect::<DecodeResult<_>>()?,
        enum_labels: s.enumeration.clone(),
        json_schema: s.json_schema.clone(),
        builtin: false,
    })
}

/// One attribute of a layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutAttr {
    pub id: i32,
    pub domain: Domain,
}

/// One representation as the record codec needs it: attributes in storage order, the first `n_fixed` fixed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordLayout {
    pub repid: u32,
    pub n_fixed: usize,
    pub attrs: Vec<LayoutAttr>,
}

impl RecordLayout {
    /// The layout of records written with `repid` (the class's current representation or an older one).
    pub fn for_repid(
        version: VersionLine,
        class: &ClassObject,
        repid: u32,
    ) -> DecodeResult<RecordLayout> {
        if repid as i32 == class.repid {
            let attrs = class
                .attributes
                .iter()
                .map(|a| {
                    Ok(LayoutAttr {
                        id: a.id,
                        domain: schema_domain(version, &a.domain)?,
                    })
                })
                .collect::<DecodeResult<_>>()?;
            return Ok(RecordLayout {
                repid,
                n_fixed: class.fixed_count.max(0) as usize,
                attrs,
            });
        }
        let rep = class
            .representations
            .iter()
            .find(|r| r.id as u32 == repid)
            .ok_or_else(|| DecodeError {
                at: 0,
                what: format!("class has no representation {repid}"),
            })?;
        let attrs = rep
            .attributes
            .iter()
            .map(|a| {
                Ok(LayoutAttr {
                    id: a.id,
                    domain: schema_domain(version, &a.domain)?,
                })
            })
            .collect::<DecodeResult<_>>()?;
        Ok(RecordLayout {
            repid,
            n_fixed: rep.fixed_count.max(0) as usize,
            attrs,
        })
    }

    pub fn n_variable(&self) -> usize {
        self.attrs.len() - self.n_fixed
    }

    /// Offsets of the fixed attributes inside the fixed area and the area's length (aligned to 4).
    /// A fixed attribute whose domain has no fixed size in `version` is a layout error.
    pub fn fixed_locations(&self, version: VersionLine) -> DecodeResult<(Vec<usize>, usize)> {
        let mut locs = Vec::with_capacity(self.n_fixed);
        let mut off = 0;
        for a in &self.attrs[..self.n_fixed] {
            locs.push(off);
            off += a.domain.disk_size(version).ok_or_else(|| DecodeError {
                at: 0,
                what: format!(
                    "attribute {} ({:?}) is variable in {version:?} but fixed in the layout",
                    a.id, a.domain.ty
                ),
            })?;
        }
        Ok((locs, off.div_ceil(4) * 4))
    }
}
