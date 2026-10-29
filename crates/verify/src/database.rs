//! Opens one database for verification: its volumes, release line, boot parameters and every class object.
//!
//! Input: the first volume's path, how to find the others, and the active log's path (the release line is the log
//! header's `db_compatibility`). Output: [`Database`]; [`vacuum_horizon`] of a source, against which the target
//! rules judge heap pages (workspace#501). Read only.
//! Source: G1 workspace#481 (volumes by header chain); `cli::migrate` pre-check (line from the log header);
//! `catalog::source_classes` (class objects from the root heap).

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use fastmig_catalog::source_classes::{SourceClass, read_classes};
use fastmig_catalog::system_table::SystemTable;
use fastmig_ondisk::boot_parm::BootDbParm;
use fastmig_ondisk::heap_rewrite::VacuumHorizon;
use fastmig_ondisk::log_header::{self, LogHeader};
use fastmig_ondisk::volume_set::{Access, Locate, VolumeSet};
use fastmig_ondisk::{Oid, SourceVersion, VersionLine};

/// Source rules (vacuum residue allowed) or target rules (vacuum residue is a defect, G3 Q5 (v), Q2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Source,
    Target,
}

pub struct Database {
    pub vols: VolumeSet,
    /// The release that wrote the database (its header layouts, workspace#529).
    pub version: SourceVersion,
    pub line: VersionLine,
    pub role: Role,
    pub boot: BootDbParm,
    pub classes: Vec<SourceClass>,
    /// Class OID → index into `classes`.
    pub by_oid: HashMap<Oid, usize>,
    /// The lob-base-path directory (`file:` stripped), when the database has one.
    pub lob_base: Option<PathBuf>,
    /// The active log's next MVCCID; every heap page's `max_mvccid` must precede it.
    pub mvcc_next_id: u64,
    /// Target rules: the source's vacuum horizon, which names the pages that had to be patched (workspace#499).
    /// `None` when the target is verified without its source.
    pub source_horizon: Option<VacuumHorizon>,
}

fn read_log(log: &Path) -> io::Result<LogHeader> {
    let page = std::fs::read(log)?;
    log_header::read_source(&page)
        .map(|(_, lh)| lh)
        .map_err(|e| io::Error::other(format!("{}: {} at {}", log.display(), e.what, e.at)))
}

/// The release that wrote the database and the codec line its pages are read with.
fn version_of(lh: &LogHeader) -> io::Result<(SourceVersion, VersionLine)> {
    SourceVersion::identify(lh.db_compatibility, &lh.db_release)
        .and_then(|v| Some((v, v.line()?)))
        .ok_or_else(|| {
            io::Error::other(format!(
                "no reader for this database: db_compatibility {} db_release {:?} (`cubrid-fastmig plan` names the \
                 ticket that opens it)",
                lh.db_compatibility, lh.db_release
            ))
        })
}

/// The vacuum horizon of the database with this first volume and active log.
pub fn vacuum_horizon(
    first_volume: &Path,
    locate: Locate,
    log: &Path,
) -> io::Result<VacuumHorizon> {
    vacuum_horizon_with_log(first_volume, locate, &read_log(log)?)
}

/// [`vacuum_horizon`] from a log header already read (a caller holding the log locked must not reopen it: closing any
/// descriptor of the file drops the lock).
pub fn vacuum_horizon_with_log(
    first_volume: &Path,
    locate: Locate,
    lh: &LogHeader,
) -> io::Result<VacuumHorizon> {
    let (version, _) = version_of(lh)?;
    let vols = VolumeSet::open(
        first_volume,
        locate,
        Access::ReadOnly,
        version.header_layout(),
    )?;
    let (_, boot) = BootDbParm::read(&vols)?;
    VacuumHorizon::read(&vols, lh, boot.vacuum_data_vfid)
}

impl Database {
    pub fn open(
        first_volume: &Path,
        locate: Locate,
        log: &Path,
        role: Role,
        lob_base: Option<PathBuf>,
    ) -> io::Result<Database> {
        Self::open_with_log(first_volume, locate, &read_log(log)?, role, lob_base)
    }

    /// [`Database::open`] from a log header already read (see [`vacuum_horizon_with_log`]).
    pub fn open_with_log(
        first_volume: &Path,
        locate: Locate,
        lh: &LogHeader,
        role: Role,
        lob_base: Option<PathBuf>,
    ) -> io::Result<Database> {
        let (version, line) = version_of(lh)?;
        let vols = VolumeSet::open(
            first_volume,
            locate,
            Access::ReadOnly,
            version.header_layout(),
        )?;
        let (_, boot) = BootDbParm::read(&vols)?;
        let classes = read_classes(&vols, line)?;
        let by_oid = classes
            .iter()
            .enumerate()
            .map(|(i, c)| (c.oid, i))
            .collect();
        Ok(Database {
            vols,
            version,
            line,
            role,
            boot,
            classes,
            by_oid,
            lob_base,
            mvcc_next_id: lh.mvcc_next_id,
            source_horizon: None,
        })
    }

    pub fn class_name(&self, oid: Oid) -> String {
        match self.by_oid.get(&oid) {
            Some(&i) => String::from_utf8_lossy(&self.classes[i].class.name).into_owned(),
            None if oid == self.boot.rootclass_oid => "<root>".into(),
            None => format!("<class {}|{}|{}>", oid.volid, oid.pageid, oid.slotid),
        }
    }

    /// The class of a system table, by its develop or its source name.
    pub fn system_table(&self, table: SystemTable) -> Option<&SourceClass> {
        let names = [table.develop_name(), table.source_name()];
        self.classes
            .iter()
            .find(|c| names.iter().flatten().any(|n| c.class.name == n.as_bytes()))
    }
}
