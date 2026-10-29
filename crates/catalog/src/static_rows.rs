//! The rows of the static catalog tables, each written to its new heap: `_db_data_type` and `dual` from the
//! catalog template, `_db_charset` and `_db_collation` from the source rows in the develop layout.
//!
//! Input: the source `_db_charset` and `_db_collation` rows (`class_rows::read_rows`), the target classes, the
//! catalog template, the migrate start time, the row allocator. Output: `(table, Row)`s with new OIDs.
//! Source: workspace#482 decisions 1 (static tables are reallocated), 2 (`_db_data_type` and `dual` rows from the
//! template, ADR 0011), 3 (`_db_charset`/`_db_collation` keep the source rows: develop checks `_db_collation`
//! against the loaded collations at boot); R4 § 2 (both tables have the same columns in 11.4 and develop).

use fastmig_ondisk::Oid;
use fastmig_ondisk::value::{DecodeError, DecodeResult, Value};

use crate::class_rows::Row;
use crate::same_slot_rows::{rewrite, template_rows};
use crate::target_classes::TargetClass;
use crate::template::CatalogTemplate;

/// Tables whose rows come from the template.
const FROM_TEMPLATE: [&str; 2] = ["_db_data_type", "dual"];
/// Tables whose rows come from the source, by name in both lines.
pub const FROM_SOURCE: [&str; 2] = ["_db_charset", "_db_collation"];

fn err(what: String) -> DecodeError {
    DecodeError { at: 0, what }
}

/// `source(table)` gives the source rows of a [`FROM_SOURCE`] table.
pub fn static_rows(
    source: &dyn Fn(&str) -> Vec<Row>,
    targets: &[TargetClass],
    template: &CatalogTemplate,
    now: &Value,
    alloc: &mut dyn FnMut(&'static str) -> Oid,
) -> DecodeResult<Vec<(&'static str, Row)>> {
    let mut out = Vec::new();
    for table in FROM_TEMPLATE {
        for r in template_rows(template, table)? {
            out.push((
                table,
                Row {
                    oid: alloc(table),
                    values: r.values,
                },
            ));
        }
    }
    for table in FROM_SOURCE {
        let t = &targets
            .iter()
            .find(|t| t.class.name == table.as_bytes())
            .ok_or_else(|| err(format!("no target class {table}")))?
            .class;
        for r in source(table) {
            let mut row = rewrite(table, table, t, &r, now)?;
            row.oid = alloc(table);
            out.push((table, row));
        }
    }
    Ok(out)
}
