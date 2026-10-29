//! The pre-check on the source's collations: a built-in collation the target engine does not have under the same
//! id and name refuses the migration; a user-locale collation is listed with a warning.
//!
//! Input: the source `_db_collation` rows (`class_rows::read_rows`) and the target's catalog template. Output:
//! refusals and warnings, for the plan's pre-check.
//! Source: workspace#482 decision 3 (develop compares `_db_collation` with the loaded collations at boot,
//! `boot_sr.c:2627-2646`, and refuses to start on a mismatch); the template's `_db_collation` rows are the target
//! engine's built-in collations (ADR 0011).

use fastmig_ondisk::value::{DecodeResult, Value};

use crate::class_rows::Row;
use crate::same_slot_rows::template_rows;
use crate::system_rows::collation;
use crate::system_table::SystemTable;
use crate::template::CatalogTemplate;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CollationCheck {
    pub refusals: Vec<String>,
    pub warnings: Vec<String>,
}

fn id_name(r: &Row) -> (Option<i32>, String) {
    let id = match collation::coll_id(&r.values) {
        Some(Value::Integer(i)) => Some(*i),
        _ => None,
    };
    let name =
        String::from_utf8_lossy(collation::coll_name(&r.values).unwrap_or_default()).into_owned();
    (id, name)
}

pub fn check_collations(
    source: &[Row],
    template: &CatalogTemplate,
) -> DecodeResult<CollationCheck> {
    let target: Vec<(Option<i32>, String)> = template_rows(template, SystemTable::DbCollation)?
        .iter()
        .map(id_name)
        .collect();
    let mut c = CollationCheck::default();
    for r in source {
        let (id, name) = id_name(r);
        match collation::built_in(&r.values) {
            Some(Value::Integer(0)) => c.warnings.push(format!(
                "collation {name} (id {id:?}) comes from a user locale library; the target engine needs the same library"
            )),
            _ if !target.contains(&(id, name.clone())) => c.refusals.push(format!(
                "built-in collation {name} (id {id:?}) is not a built-in collation of the target engine under that id"
            )),
            _ => {}
        }
    }
    Ok(c)
}
