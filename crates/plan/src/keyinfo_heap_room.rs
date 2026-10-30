//! Room in volume 0 for the TDE keyinfo heap a 10.2 source gets: volume 0's free sectors now plus those the old
//! B-tree files free before the heap is created.
//!
//! Input: the source volume set and its file tracker. Output: [`Volume0Room`]; `plan` shows it and refuses a source
//! with no room.
//! Source: the keyinfo heap must lie in volume 0, which the server alone has mounted when it loads the keys
//! (`boot_restart_server`, boot_sr.c:2306-2335 develop @682b080b2; workspace#515); `catalog_regeneration::prepare`
//! creates it right after `free_btree_files` and the engine reserves sectors volume 0 first
//! (`disk_reserve_from_cache_vols`, release/10.2 and develop). A 10.2 createdb puts its system B-trees in volume 0
//! (17 on a minimal 4 KiB-page DB whose volume 0 is the 64-sector minimum), and they cannot be dropped
//! (workspace#550).

use std::collections::BTreeSet;
use std::io;

use fastmig_ondisk::boot_parm::BootDbParm;
use fastmig_ondisk::file_header::FileType;
use fastmig_ondisk::file_table::FileTable;
use fastmig_ondisk::volume_set::VolumeSet;
use fastmig_ondisk::{file_tracker, sector_table};

/// Volume 0 sectors the keyinfo heap can take (it needs one).
#[derive(Clone, Copy, Debug)]
pub struct Volume0Room {
    /// Free in the sector table now.
    pub free: i32,
    /// Reserved by B-tree and overflow-key files, all freed before the heap is created.
    pub freed_btree: usize,
}

impl Volume0Room {
    pub fn read(vols: &VolumeSet) -> io::Result<Self> {
        let v0 = vols.volume(0)?;
        let free = v0.header.nsect_total - sector_table::used_sectors(vols, 0)?.len() as i32;
        let (items, _) = file_tracker::read(vols, BootDbParm::read(vols)?.1.trk_vfid)?;
        let btree = [FileType::Btree as i16, FileType::BtreeOverflowKey as i16];
        let mut sectors = BTreeSet::new();
        for it in items.iter().filter(|it| btree.contains(&it.file_type_raw)) {
            let t = FileTable::read(vols, it.vfid)?;
            sectors.extend(
                t.sectors
                    .iter()
                    .filter(|s| s.vsid.volid == 0)
                    .map(|s| s.vsid.sectid),
            );
        }
        Ok(Volume0Room {
            free,
            freed_btree: sectors.len(),
        })
    }

    pub fn total(&self) -> i64 {
        self.free as i64 + self.freed_btree as i64
    }
}
