//! The representation substructures of a class object: the layouts of the class's older record versions.
//!
//! Input: one element of the class's `representations` set. Output: [`Representation`] with its [`RepAttribute`]s.
//! Source: engine `transform_cl.c:representation_to_disk` / `disk_to_representation` (vars: attributes,
//! properties; fixed 16 B: id, fixed_count, variable_count, 0) and `repattribute_to_disk` / `disk_to_repattribute`
//! (var: domain; fixed 8 B: id, type); same in release/11.4 @46212adcd and develop @5b1867174; R3 § 2.2.

use super::domain::{self, SchemaDomain};
use super::or_be::{Structure, within};
use super::sub_set;
use crate::value::{DecodeResult, StringCompression};

/// One older representation (repid) of the class.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Representation {
    pub id: i32,
    pub fixed_count: i32,
    pub variable_count: i32,
    pub attributes: Vec<RepAttribute>,
}

/// One attribute of an older representation, in storage order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepAttribute {
    pub id: i32,
    pub type_id: i32,
    pub domain: Vec<SchemaDomain>,
}

impl Representation {
    fn decode(elem: &[u8], c: StringCompression) -> DecodeResult<Representation> {
        let s = Structure::new(elem, 2)?;
        let mut f = s.fixed();
        let id = f.i32()?;
        let fixed_count = f.i32()?;
        let variable_count = f.i32()?;
        let attributes = sub_set::elements(s.var(0))?
            .into_iter()
            .map(|e| {
                let a = Structure::new(e, 1)?;
                let mut f = a.fixed();
                Ok(RepAttribute {
                    id: f.i32()?,
                    type_id: f.i32()?,
                    domain: domain::list(a.var(0), c)?,
                })
            })
            .collect::<DecodeResult<_>>();
        Ok(Representation {
            id,
            fixed_count,
            variable_count,
            attributes: within(attributes, &format!("representation {id}"))?,
        })
    }
}

pub(crate) fn list(field: &[u8], c: StringCompression) -> DecodeResult<Vec<Representation>> {
    sub_set::elements(field)?
        .into_iter()
        .map(|e| Representation::decode(e, c))
        .collect()
}
