//! The method-related substructures of a class object (methods, signatures, arguments, method files) and
//! resolutions. System classes such as `db_user` carry class methods; user classes in practice do not.
//!
//! Input: one element of the `methods`, `class_methods`, `method_files` or `resolutions` set.
//! Output: [`Method`], [`MethodFile`], [`Resolution`].
//! Source: engine `transform_cl.c` `method_to_disk` (vars: name, signatures, properties; fixed: class OID, id),
//! `methsig_to_disk` (vars: function_name, sql_definition, return value, arguments; fixed: num_args),
//! `metharg_to_disk` (var: domain; fixed: type, index), `methfile_to_disk` (vars: name, properties; fixed: class
//! OID), `resolution_to_disk` (vars: name, alias; fixed: class OID, name space); same in release/11.4 @46212adcd and
//! develop @5b1867174.

use super::domain::{self, SchemaDomain};
use super::or_be::{Structure, opt_string, read_oid, within};
use super::sub_set;
use crate::Oid;
use crate::value::{DecodeResult, StringCompression};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Method {
    pub class_oid: Oid,
    pub id: i32,
    pub name: Vec<u8>,
    pub signatures: Vec<MethodSig>,
    /// Packed property sequence; empty for none.
    pub properties: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MethodSig {
    pub num_args: i32,
    pub function_name: Option<Vec<u8>>,
    pub sql_definition: Option<Vec<u8>>,
    pub return_value: Vec<MethodArg>,
    pub arguments: Vec<MethodArg>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MethodArg {
    pub type_id: i32,
    pub index: i32,
    pub domain: Vec<SchemaDomain>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MethodFile {
    pub class_oid: Oid,
    pub name: Option<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolution {
    pub class_oid: Oid,
    pub name_space: i32,
    pub name: Option<Vec<u8>>,
    pub alias: Option<Vec<u8>>,
}

fn args(field: &[u8], c: StringCompression) -> DecodeResult<Vec<MethodArg>> {
    sub_set::elements(field)?
        .into_iter()
        .map(|e| {
            let s = Structure::new(e, 1)?;
            let mut f = s.fixed();
            Ok(MethodArg {
                type_id: f.i32()?,
                index: f.i32()?,
                domain: domain::list(s.var(0), c)?,
            })
        })
        .collect()
}

fn signature(elem: &[u8], c: StringCompression) -> DecodeResult<MethodSig> {
    let s = Structure::new(elem, 4)?;
    Ok(MethodSig {
        num_args: s.fixed().i32()?,
        function_name: opt_string(s.var(0), c)?,
        sql_definition: opt_string(s.var(1), c)?,
        return_value: args(s.var(2), c)?,
        arguments: args(s.var(3), c)?,
    })
}

pub(crate) fn methods(field: &[u8], c: StringCompression) -> DecodeResult<Vec<Method>> {
    sub_set::elements(field)?
        .into_iter()
        .map(|e| {
            let s = Structure::new(e, 3)?;
            let mut f = s.fixed();
            let class_oid = read_oid(&mut f)?;
            let id = f.i32()?;
            let name = opt_string(s.var(0), c)?.unwrap_or_default();
            let signatures = sub_set::elements(s.var(1))?
                .into_iter()
                .map(|e| signature(e, c))
                .collect::<DecodeResult<_>>();
            Ok(Method {
                class_oid,
                id,
                signatures: within(
                    signatures,
                    &format!("method {}", String::from_utf8_lossy(&name)),
                )?,
                name,
                properties: s.var(2).to_vec(),
            })
        })
        .collect()
}

pub(crate) fn method_files(field: &[u8], c: StringCompression) -> DecodeResult<Vec<MethodFile>> {
    sub_set::elements(field)?
        .into_iter()
        .map(|e| {
            let s = Structure::new(e, 2)?;
            Ok(MethodFile {
                class_oid: read_oid(&mut s.fixed())?,
                name: opt_string(s.var(0), c)?,
            })
        })
        .collect()
}

pub(crate) fn resolutions(field: &[u8], c: StringCompression) -> DecodeResult<Vec<Resolution>> {
    sub_set::elements(field)?
        .into_iter()
        .map(|e| {
            let s = Structure::new(e, 2)?;
            let mut f = s.fixed();
            Ok(Resolution {
                class_oid: read_oid(&mut f)?,
                name_space: f.i32()?,
                name: opt_string(s.var(0), c)?,
                alias: opt_string(s.var(1), c)?,
            })
        })
        .collect()
}
