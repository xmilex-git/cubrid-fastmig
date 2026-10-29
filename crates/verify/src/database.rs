//! Opens one database for verification: its volumes, release line, boot parameters and every class object.
//!
//! Input: the first volume's path, how to find the others, and the active log's path (the release line is the log
//! header's `db_compatibility`). Output: [`Database`]. Read only.
//! Source: G1 workspace#481 (volumes by header chain); `cli::migrate` pre-check (line from the log header);
//! `catalog::source_classes` (class objects from the root heap).

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use fastmig_catalog::source_classes::{SourceClass, read_classes};
use fastmig_ondisk::boot_parm::BootDbParm;
use fastmig_ondisk::log_header::{self, LogHeader};
use fastmig_ondisk::volume_set::{Access, Locate, VolumeSet};
use fastmig_ondisk::{Oid, VersionLine};

/// Source rules (vacuum residue allowed) or target rules (vacuum residue is a defect, G3 Q5 (v), Q2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Source,
    Target,
}

pub struct Database {
    pub vols: VolumeSet,
    pub line: VersionLine,
    pub role: Role,
    pub boot: BootDbParm,
    pub classes: Vec<SourceClass>,
    /// Class OID → index into `classes`.
    pub by_oid: HashMap<Oid, usize>,
    /// The lob-base-path directory (`file:` stripped), when the database has one.
    pub lob_base: Option<PathBuf>,
}

impl Database {
    pub fn open(
        first_volume: &Path,
        locate: Locate,
        log: &Path,
        role: Role,
        lob_base: Option<PathBuf>,
    ) -> io::Result<Database> {
        let page = std::fs::read(log)?;
        if page.len() < log_header::LOG_HEADER_SIZE {
            return Err(io::Error::other(format!("{}: too short", log.display())));
        }
        let lh = LogHeader::read(&page);
        if lh.magic != log_header::MAGIC {
            return Err(io::Error::other(format!(
                "{}: not an active log",
                log.display()
            )));
        }
        let line = VersionLine::from_db_compatibility(lh.db_compatibility).ok_or_else(|| {
            io::Error::other(format!(
                "unsupported db_compatibility {}",
                lh.db_compatibility
            ))
        })?;
        let vols = VolumeSet::open(first_volume, locate, Access::ReadOnly)?;
        let (_, boot) = BootDbParm::read(&vols)?;
        let classes = read_classes(&vols, line)?;
        let by_oid = classes
            .iter()
            .enumerate()
            .map(|(i, c)| (c.oid, i))
            .collect();
        Ok(Database {
            vols,
            line,
            role,
            boot,
            classes,
            by_oid,
            lob_base,
        })
    }

    pub fn class_name(&self, oid: Oid) -> String {
        match self.by_oid.get(&oid) {
            Some(&i) => String::from_utf8_lossy(&self.classes[i].class.name).into_owned(),
            None if oid == self.boot.rootclass_oid => "<root>".into(),
            None => format!("<class {}|{}|{}>", oid.volid, oid.pageid, oid.slotid),
        }
    }

    /// The class whose unique name is one of `names` (11.4 and develop spell some system classes differently).
    pub fn class_named(&self, names: &[&str]) -> Option<&SourceClass> {
        self.classes
            .iter()
            .find(|c| names.iter().any(|n| c.class.name == n.as_bytes()))
    }
}
