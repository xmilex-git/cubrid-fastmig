//! Frees the source catalog files the regenerated catalog replaces, before anything new is allocated: the catalog
//! file (representation directory) and the heaps, with their overflow heaps, of the reallocated and dropped system
//! tables. B-tree files are the index rebuild's (workspace#503).
//!
//! Input: the target's [`SpaceManager`] (the source bytes still in place) and the [`SourceCatalog`]. Output: the files
//! destroyed, with their types.
//! Source: workspace#482 decisions 1 (which tables are reallocated or dropped) and 10.2 (free before the first new
//! allocation, so their sectors are reused; R5 F1-F4); the catalog's extendible hash (`ctid.xhid`) stays: develop
//! only creates it (`system_catalog.c` `catalog_create`) and never reads it (workspace#492). Same list as
//! `dry_run` section 2 without the B-trees.

use std::io;

use fastmig_ondisk::boot_parm::BootDbParm;
use fastmig_ondisk::file_alloc::SpaceManager;
use fastmig_ondisk::file_header::{FileDescriptor, FileHeader, FileType};
use fastmig_ondisk::{Oid, Vfid, file_tracker};

use crate::classify::ClassKind;
use crate::source_catalog::SourceCatalog;
use crate::system_classes::SystemClassRole;

pub fn free_old_files(
    space: &SpaceManager,
    source: &SourceCatalog,
) -> io::Result<Vec<(Vfid, FileType)>> {
    let vs = space.volumes();
    let (_, boot) = BootDbParm::read(vs)?;
    let freed_class = |oid: Oid| {
        source.plans.iter().any(|p| {
            p.oid == oid
                && matches!(
                    p.kind,
                    ClassKind::System(SystemClassRole::Reallocated | SystemClassRole::Dropped)
                )
        })
    };
    let (items, _) = file_tracker::read(vs, boot.trk_vfid)?;
    let mut out = Vec::new();
    for it in &items {
        let h = FileHeader::read(&vs.read_page(it.vfid.header_vpid())?);
        let free = match (h.file_type(), h.descriptor()) {
            (Some(FileType::Catalog), _) => true,
            (
                Some(FileType::Heap | FileType::HeapReuseSlots),
                FileDescriptor::Heap { class_oid, .. },
            )
            | (
                Some(FileType::MultipageObjectHeap),
                FileDescriptor::HeapOverflow { class_oid, .. },
            ) => freed_class(class_oid),
            _ => false,
        };
        if let (true, Some(t)) = (free, h.file_type()) {
            out.push((it.vfid, t));
        }
    }
    for (vfid, _) in &out {
        space.destroy_file(*vfid)?;
    }
    Ok(out)
}
