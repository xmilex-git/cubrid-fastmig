//! Maps 11.4 domains and values to their develop form: NCHAR(n) → CHAR(n), VARNCHAR(n) → VARCHAR(n), recursively
//! through collection element domains and element tags. Codeset, collation, precision stay.
//!
//! Input: a [`Domain`] or [`Value`] decoded from an 11.4 database. Output: the same in develop terms; types that did
//! not change pass through.
//! Source: G2 decision 5 (workspace#482); develop `src/compat/dbtype_def.h:779` (`DB_TYPE_NCHAR_DEPRECATED`),
//! `src/parser/csql_grammar.y:37` (`PT_TYPE_NCHAR` = `PT_TYPE_CHAR`); R2 § 2 (VARNCHAR bytes = VARCHAR bytes).

use crate::value::db_type::DbType;
use crate::value::db_value::{SetElement, SetValue, Value};
use crate::value::domain::{Domain, MAX_VARCHAR_PRECISION, MAX_VARNCHAR_PRECISION_11_4};

/// The develop domain for an 11.4 domain.
pub fn domain_to_develop(d: &Domain) -> Domain {
    let mut out = d.clone();
    match d.ty {
        DbType::Nchar => out.ty = DbType::Char,
        DbType::Varnchar => {
            out.ty = DbType::Varchar;
            // the unbounded VARNCHAR becomes the unbounded VARCHAR
            if d.precision == MAX_VARNCHAR_PRECISION_11_4 {
                out.precision = MAX_VARCHAR_PRECISION;
            }
        }
        _ => {}
    }
    out.set_domain = d.set_domain.iter().map(domain_to_develop).collect();
    out
}

/// The develop value for an 11.4 value (NCHAR/VARNCHAR variants renamed, set domains and tags mapped).
pub fn value_to_develop(v: &Value) -> Value {
    match v {
        Value::Nchar(s) => Value::Char(s.clone()),
        Value::Varnchar(s) => Value::Varchar(s.clone()),
        Value::Set(s) => Value::Set(SetValue {
            ty: s.ty,
            domain: s.domain.as_ref().map(domain_to_develop),
            elements: s
                .elements
                .iter()
                .map(|e| SetElement {
                    tag: e.tag.as_ref().map(domain_to_develop),
                    value: value_to_develop(&e.value),
                })
                .collect(),
        }),
        other => other.clone(),
    }
}

/// Whether values of this domain are encoded differently in develop (CHAR, NCHAR, NUMERIC, a collection of them, or
/// a collection without an element type, whose tagged elements may be any of them): the test for a re-encoded class
/// (R2 § 3 R7.1, workspace#474; untyped collections workspace#499, release/11.4 `csql_grammar.y` `data_type: set_type`).
pub fn changes_in_develop(d: &Domain) -> bool {
    matches!(d.ty, DbType::Char | DbType::Nchar | DbType::Numeric)
        || matches!(d.ty, DbType::Set | DbType::Multiset | DbType::Sequence)
            && d.set_domain.is_empty()
        || d.set_domain.iter().any(changes_in_develop)
}
