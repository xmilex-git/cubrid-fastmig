//! Empties the HA apply-info table of a target (`_db_ha_apply_info`; `db_ha_apply_info` before develop) on both the
//! catalog-regeneration and the same-version path.
//!
//! A row is one slave applier's position in a master's log (`required_lsa`, `committed_lsa`, `append_lsa`, ...),
//! found by (`db_name`, `copied_log_path`) and trusted when its `db_creation_time` equals the copied master log
//! header's (`log_applier.c` `la_get_last_ha_applied_info`). migrate keeps `db_creation` (G4) and starts the target
//! log at a later page (`copy::new_log_start`), so a carried row sends the applier into a log chain the target does
//! not have: applylogdb stops with "cannot find archive log for 9205 page" and -81 when the target pair copies to
//! the source pair's path (workspace#590 run-05). Without a row the applier starts at the copied log's end and writes
//! its own row, the start `restoreslave` gives a slave it rebuilds (`util_sa.c` `restoreslave`: every row deleted,
//! then one inserted at the backup's position). The source keeps its rows. ADR 0002 (amended by workspace#590).
//!
//! Input: the source classes and line (the rows to delete), an open [`HeapWriter`] on the table's heap. Output: the
//! rows deleted from the target heap; [`rows`] counts them for the plan.
//! Source: engine `log_applier.c:la_get_last_ha_applied_info`, `util_sa.c:restoreslave` (develop @682b080b2).

use std::io;

use fastmig_ondisk::heap_rewrite::HeapWriter;
use fastmig_ondisk::volume_set::VolumeSet;
use fastmig_ondisk::{Oid, VersionLine};

use crate::class_rows::read_rows;
use crate::source_classes::SourceClass;
use crate::system_table::SystemTable;

/// The `rows_written` entry of the rows deleted (`report.json` catalog).
pub const RESET_ROWS: &str = "reset HA apply info rows";

/// The table's class in a catalog of `line`; `None` when the catalog has none or its heap was never created.
pub fn class(classes: &[SourceClass], line: VersionLine) -> Option<&SourceClass> {
    let name = SystemTable::DbHaApplyInfo.name_in(line)?;
    classes
        .iter()
        .find(|c| c.class.name == name.as_bytes() && !c.class.heap.is_null())
}

/// OIDs of the live rows of the table in a source of `line`.
pub fn rows(vs: &VolumeSet, line: VersionLine, classes: &[SourceClass]) -> io::Result<Vec<Oid>> {
    match class(classes, line) {
        Some(c) => Ok(read_rows(vs, line, c)?.into_iter().map(|r| r.oid).collect()),
        None => Ok(Vec::new()),
    }
}

/// Deletes `oids` through `w`, the writer of the table's heap; returns how many.
pub fn reset(w: &mut HeapWriter, oids: &[Oid]) -> io::Result<usize> {
    for &o in oids {
        w.delete(o)?;
    }
    Ok(oids.len())
}
