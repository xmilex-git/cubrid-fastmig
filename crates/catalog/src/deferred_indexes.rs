//! The deferred indexes of a database: every function or filter index of a user class, which the tool does not
//! build and hands to the user as DDL.
//!
//! Input: the source class objects and their line. Output: `(class, constraint)` per deferred index, partitions'
//! copies included (`output_sql::deferred_indexes_sql` and [`report_name`] skip those).
//! Source: decision G5 workspace#486; ADR 0006.

use std::io;

use fastmig_ondisk::VersionLine;
use fastmig_ondisk::class_object::{ClassObject, Constraint, class_properties};

use crate::source_classes::SourceClass;

/// Engine `SM_CLASSFLAG_SYSTEM`.
const CLASSFLAG_SYSTEM: i32 = 1;

pub fn deferred_indexes(
    classes: &[SourceClass],
    line: VersionLine,
) -> io::Result<Vec<(&ClassObject, Constraint)>> {
    let mut out = Vec::new();
    for c in classes
        .iter()
        .filter(|c| c.class.flags & CLASSFLAG_SYSTEM == 0)
    {
        let props = class_properties(&c.class.properties, line).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} properties: {e}", String::from_utf8_lossy(&c.class.name)),
            )
        })?;
        out.extend(
            props
                .constraints
                .into_iter()
                .filter(|k| k.has_filter || k.has_function)
                .map(|k| (&c.class, k)),
        );
    }
    Ok(out)
}

/// `class.index` for the plan's report; `None` for a partition's copy (the engine recreates it with the
/// partitioned class's index).
pub fn report_name(class: &ClassObject, k: &Constraint) -> Option<String> {
    (!class.partitions.iter().any(|p| p.pname.is_some())).then(|| {
        format!(
            "{}.{}",
            String::from_utf8_lossy(&class.name),
            String::from_utf8_lossy(&k.name)
        )
    })
}
