//! The boot parameter record (`BOOT_DB_PARM`): where the file tracker, root class heap, class name hash, catalog,
//! vacuum files and TDE key heap live, plus the volume counts.
//!
//! Input: the volume set (volume 0 header `boot_hfid` → that heap's header page). Output: [`BootDbParm`] and the
//! OID of its record.
//! Source: engine `src/transaction/boot_sr.c:122-142` (develop @5b1867174) = :113-132 (release/11.4 @46212adcd),
//! stored as raw struct bytes by `boot_create_all_volumes` (:5074-5091) into the boot heap. Size 136 B. Offsets
//! checked on T1 fm114_clean (slot 1 of heap header page 0|129, workspace#490).

use std::io;

use crate::ids::{Ehid, Hfid, Oid, Vfid};
use crate::le_field::{get_i16, get_i32, put_i16, put_i32};
use crate::page::{db_area, db_area_mut};
use crate::spage::{RecordType, SlottedPage, SpageEditor};
use crate::volume_set::VolumeSet;

/// `sizeof (BOOT_DB_PARM)`.
pub const BOOT_DB_PARM_SIZE: usize = 136;

/// Engine `CTID`: catalog file, its extendible hash, its header page. Size 24 B: `vfid` @0, `xhid` @8, `hpgid` @20.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ctid {
    pub vfid: Vfid,
    pub xhid: Ehid,
    pub hpgid: i32,
}

/// Engine `BOOT_DB_PARM`. Offsets: `trk_vfid` @0, `hfid` @8, `rootclass_hfid` @20, `classname_table` @32,
/// `ctid` @44, `query_vfid` @68, `rootclass_name` char[10] @76, `rootclass_oid` @88, `nvols` @96, `temp_nvols` @98,
/// `last_volid` @100, `temp_last_volid` @102, `vacuum_log_block_npages` @104, `vacuum_data_vfid` @108,
/// `dropped_files_vfid` @116, `tde_keyinfo_hfid` @124.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BootDbParm {
    /// The file tracker.
    pub trk_vfid: Vfid,
    /// The heap holding this record.
    pub hfid: Hfid,
    /// The root class heap: class objects.
    pub rootclass_hfid: Hfid,
    /// Class name → class OID extendible hash.
    pub classname_table: Ehid,
    pub ctid: Ctid,
    pub query_vfid: Vfid,
    pub rootclass_name: String,
    pub rootclass_oid: Oid,
    pub nvols: i16,
    pub temp_nvols: i16,
    pub last_volid: i16,
    pub temp_last_volid: i16,
    /// `vacuum_log_block_npages` (default 31); the new log's first page is aligned to it (G4, workspace#485).
    pub vacuum_log_block_npages: i32,
    pub vacuum_data_vfid: Vfid,
    pub dropped_files_vfid: Vfid,
    pub tde_keyinfo_hfid: Hfid,
}

impl BootDbParm {
    pub fn decode(r: &[u8]) -> Self {
        let name = &r[76..86];
        let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
        BootDbParm {
            trk_vfid: Vfid::read(r, 0),
            hfid: Hfid::read(r, 8),
            rootclass_hfid: Hfid::read(r, 20),
            classname_table: Ehid::read(r, 32),
            ctid: Ctid {
                vfid: Vfid::read(r, 44),
                xhid: Ehid::read(r, 52),
                hpgid: get_i32(r, 64),
            },
            query_vfid: Vfid::read(r, 68),
            rootclass_name: String::from_utf8_lossy(&name[..end]).into_owned(),
            rootclass_oid: Oid::read(r, 88),
            nvols: get_i16(r, 96),
            temp_nvols: get_i16(r, 98),
            last_volid: get_i16(r, 100),
            temp_last_volid: get_i16(r, 102),
            vacuum_log_block_npages: get_i32(r, 104),
            vacuum_data_vfid: Vfid::read(r, 108),
            dropped_files_vfid: Vfid::read(r, 116),
            tde_keyinfo_hfid: Hfid::read(r, 124),
        }
    }

    /// Reads the record from the boot heap named by volume 0's header: the first record after the heap header
    /// (slot 0) of the heap header page, where `boot_create_all_volumes` inserts it into the then-empty heap.
    pub fn read(vols: &VolumeSet) -> io::Result<(Oid, Self)> {
        let hfid = vols.volume(0)?.header.boot_hfid;
        let page = vols.read_page(hfid.vpid())?;
        let sp = SlottedPage::new(db_area(&page)).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "boot heap header page is not slotted",
            )
        })?;
        for slot in 1..sp.num_slots() {
            if let Some((RecordType::Home, rec)) = sp.record(slot)
                && rec.len() == BOOT_DB_PARM_SIZE
            {
                let oid = Oid::new(hfid.vfid.volid, hfid.pageid, slot as i16);
                return Ok((oid, Self::decode(rec)));
            }
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no BOOT_DB_PARM record in the boot heap header page",
        ))
    }

    /// Engine `boot_dbparm_save_volume` for a permanent volume: `nvols` and `last_volid` rewritten in place
    /// (same record length, same slot).
    pub fn write_volume_counts(vols: &VolumeSet, nvols: i16, last_volid: i16) -> io::Result<()> {
        Self::update(vols, |rec| {
            put_i16(rec, 96, nvols);
            put_i16(rec, 100, last_volid);
        })
    }

    /// The catalog file of a regenerated catalog (`catalog_create` result): `ctid.vfid` and `ctid.hpgid` rewritten
    /// in place; `ctid.xhid` stays, develop never reads the catalog's extendible hash (workspace#492).
    pub fn write_ctid(vols: &VolumeSet, vfid: Vfid, hpgid: i32) -> io::Result<()> {
        Self::update(vols, |rec| {
            vfid.write(rec, 44);
            put_i32(rec, 64, hpgid);
        })
    }

    /// Rewrites the record in place (same length, same slot) through `f`.
    fn update(vols: &VolumeSet, f: impl FnOnce(&mut [u8])) -> io::Result<()> {
        let (oid, _) = Self::read(vols)?;
        let mut page = vols.read_page(oid.vpid())?;
        let bad = |m: &str| io::Error::new(io::ErrorKind::InvalidData, m.to_string());
        let mut ed =
            SpageEditor::new(db_area_mut(&mut page)).ok_or_else(|| bad("boot heap page"))?;
        let mut rec = match ed.view().record(oid.slotid as usize) {
            Some((RecordType::Home, r)) => r.to_vec(),
            _ => return Err(bad("BOOT_DB_PARM record moved")),
        };
        f(&mut rec);
        ed.update(oid.slotid as usize, &rec)
            .map_err(|e| bad(&format!("BOOT_DB_PARM update: {e:?}")))?;
        vols.write_page(oid.vpid(), &page)
    }
}
