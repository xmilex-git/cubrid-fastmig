//! Everything catalog regeneration reads from the source catalog, read once before the target is written: the class
//! objects with their plans, the rows of the same-slot tables, the source `_db_charset`/`_db_collation` rows and the
//! database charset.
//!
//! Input: the target's volume set while it still holds the source's bytes (right after the copy), the source
//! release line and the target line. Output: [`SourceCatalog`].
//! Source: workspace#482 decision 10 (old files are freed before anything new is allocated, so the reallocated
//! tables' rows must be read first: `_db_charset` and `_db_collation` live in heaps decision 1 reallocates).

use std::collections::HashMap;
use std::io;

use fastmig_ondisk::volume_set::VolumeSet;
use fastmig_ondisk::{SourceVersion, VersionLine};

use crate::class_rows::{Row, read_rows};
use crate::classify::{ClassPlan, classify};
use crate::owner_qualification::qualify_names;
use crate::rows_before_11_4::to_11_4_shape;
use crate::source_classes::{SourceClass, read_classes};
use crate::static_rows::FROM_SOURCE;
use crate::stored_texts::map_stored_texts;
use crate::system_classes::same_slot;
use crate::system_table::SystemTable;

pub struct SourceCatalog {
    /// The release that wrote the source (which same-slot tables it has, workspace#529).
    pub version: SourceVersion,
    pub line: VersionLine,
    pub classes: Vec<SourceClass>,
    /// One per class, same order.
    pub plans: Vec<ClassPlan>,
    /// Rows of the same-slot tables, by develop table name.
    pub same_slot: HashMap<SystemTable, Vec<Row>>,
    /// Rows of `_db_charset` and `_db_collation`.
    pub static_source: HashMap<SystemTable, Vec<Row>>,
    /// Volume 0 header `db_charset`.
    pub charset: u8,
}

fn missing(name: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("source catalog has no class {name}"),
    )
}

impl SourceCatalog {
    /// `target` is the line regeneration writes: a pre-11.4 stored procedure target splits by its rules.
    pub fn read(vs: &VolumeSet, version: SourceVersion, target: VersionLine) -> io::Result<Self> {
        let line = version.line().ok_or_else(|| {
            io::Error::other(format!("source {} has no codec line", version.name()))
        })?;
        let mut classes = read_classes(vs, line)?;
        let plans = classify(&classes, line);
        let rows = |name: &str| -> io::Result<Vec<Row>> {
            let c = classes
                .iter()
                .find(|c| c.class.name == name.as_bytes())
                .ok_or_else(|| missing(name))?;
            read_rows(vs, line, c)
        };
        let mut same_slot: HashMap<SystemTable, Vec<Row>> = same_slot(version)
            .map(|t| Ok((t, rows(t.source_name().unwrap_or_default())?)))
            .collect::<io::Result<_>>()?;
        to_11_4_shape(version, target, &mut same_slot, &classes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.what))?;
        let static_source = FROM_SOURCE
            .iter()
            .map(|t| Ok((*t, rows(t.name())?)))
            .collect::<io::Result<_>>()?;
        // old system class names in view and trigger texts become develop names (workspace#542); a text the mapping
        // refuses stays as it is: the plan reports it and migrate does not start
        let mut triggers = same_slot
            .remove(&SystemTable::DbTrigger)
            .unwrap_or_default();
        let users = same_slot
            .get(&SystemTable::DbUser)
            .map(Vec::as_slice)
            .unwrap_or_default();
        map_stored_texts(version, &mut classes, users, &mut triggers);
        same_slot.insert(SystemTable::DbTrigger, triggers);
        // a source before user schema gets owner-qualified names and texts (workspace#544); the plan reports the
        // warnings
        let charset = vs.volume(0)?.header.db_charset as u8;
        qualify_names(version, &mut classes, &mut same_slot, charset)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.what))?;
        Ok(SourceCatalog {
            version,
            line,
            plans,
            same_slot,
            static_source,
            charset,
            classes,
        })
    }
}
