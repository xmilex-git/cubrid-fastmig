//! Everything catalog regeneration reads from the source catalog, read once before the target is written: the class
//! objects with their plans, the rows of the same-slot tables, the source `_db_charset`/`_db_collation` rows and the
//! database charset.
//!
//! Input: the target's volume set while it still holds the source's bytes (right after the copy), and the source
//! release line. Output: [`SourceCatalog`].
//! Source: workspace#482 decision 10 (old files are freed before anything new is allocated, so the reallocated
//! tables' rows must be read first: `_db_charset` and `_db_collation` live in heaps decision 1 reallocates).

use std::collections::HashMap;
use std::io;

use fastmig_ondisk::VersionLine;
use fastmig_ondisk::volume_set::VolumeSet;

use crate::class_rows::{Row, read_rows};
use crate::classify::{ClassPlan, classify};
use crate::source_classes::{SourceClass, read_classes};
use crate::static_rows::FROM_SOURCE;
use crate::system_classes::SAME_SLOT;

pub struct SourceCatalog {
    pub line: VersionLine,
    pub classes: Vec<SourceClass>,
    /// One per class, same order.
    pub plans: Vec<ClassPlan>,
    /// Rows of the same-slot tables, by develop table name.
    pub same_slot: HashMap<&'static str, Vec<Row>>,
    /// Rows of `_db_charset` and `_db_collation`.
    pub static_source: HashMap<&'static str, Vec<Row>>,
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
    pub fn read(vs: &VolumeSet, line: VersionLine) -> io::Result<Self> {
        let classes = read_classes(vs, line)?;
        let plans = classify(&classes);
        let rows = |name: &str| -> io::Result<Vec<Row>> {
            let c = classes
                .iter()
                .find(|c| c.class.name == name.as_bytes())
                .ok_or_else(|| missing(name))?;
            read_rows(vs, line, c)
        };
        let same_slot = SAME_SLOT
            .iter()
            .map(|(src, dev)| Ok((*dev, rows(src)?)))
            .collect::<io::Result<_>>()?;
        let static_source = FROM_SOURCE
            .iter()
            .map(|t| Ok((*t, rows(t)?)))
            .collect::<io::Result<_>>()?;
        Ok(SourceCatalog {
            line,
            plans,
            same_slot,
            static_source,
            charset: vs.volume(0)?.header.db_charset as u8,
            classes,
        })
    }
}
