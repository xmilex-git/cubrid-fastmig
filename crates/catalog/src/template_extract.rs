//! Builds a catalog template from a database the target engine just created (`createdb`, nothing else run).
//!
//! Input: the fresh database's volume set, its release line, and the `meta` lines describing how it was made.
//! Output: a [`CatalogTemplate`] holding every class object (all are system classes in a fresh database) and the
//! rows of the system classes whose content regeneration takes from the target engine.
//! Source: ADR 0011, workspace#482 decision 1 (static rows `_db_data_type` and `dual`) and decision 2 (the
//! `INFORMATION_SCHEMA` user and the system grants come from the template).

use std::io;

use fastmig_ondisk::VersionLine;
use fastmig_ondisk::volume_set::VolumeSet;

use crate::source_classes::{read_classes, read_rows};
use crate::template::{CatalogTemplate, TemplateRecord};

/// System classes whose rows the template carries: the static rows (`_db_data_type`, `dual`), and the users,
/// passwords, authorizations and grants of a fresh database (DBA, PUBLIC, INFORMATION_SCHEMA and their grants on
/// system classes); `db_root` for its one row.
pub const ROW_CLASSES: [&str; 7] = [
    "_db_data_type",
    "dual",
    "_db_user",
    "_db_password",
    "_db_authorization",
    "_db_auth",
    "db_root",
];

pub fn extract(
    vs: &VolumeSet,
    line: VersionLine,
    meta: Vec<(String, String)>,
) -> io::Result<CatalogTemplate> {
    let classes = read_classes(vs, line)?;
    let mut t = CatalogTemplate {
        meta,
        ..Default::default()
    };
    for name in ROW_CLASSES {
        let c = classes
            .iter()
            .find(|c| c.class.name == name.as_bytes())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("no class {name} in the template DB"),
                )
            })?;
        for r in read_rows(vs, c.class.heap)? {
            t.rows.push((
                name.to_string(),
                TemplateRecord {
                    oid: r.oid,
                    bytes: r.bytes,
                },
            ));
        }
    }
    t.classes = classes
        .into_iter()
        .map(|c| TemplateRecord {
            oid: c.oid,
            bytes: c.record,
        })
        .collect();
    Ok(t)
}
