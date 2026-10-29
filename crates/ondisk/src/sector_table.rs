//! The sector allocation table (STAB) of a volume: one bit per sector saying whether a file (or the volume's own
//! system pages) owns it.
//!
//! Input: a volume header and the STAB pages. Output: which sectors are set; the STAB page with a bit set or cleared.
//! Source: engine `src/storage/disk_manager.c:194-231, 3263-3279` (`DISK_STAB_UNIT` = UINT64, LSB = lowest
//! sectid), `disk_stab_init` (:4930-5013); same in release/11.4 @46212adcd and develop @5b1867174 (R5 § 2.3).
//! The used-sector count is not stored; the engine recounts at boot.

use std::io;

use crate::ids::Vpid;
use crate::le_field::{get_u64, put_u64};
use crate::page::{IO_RESERVED_SIZE, db_pagesize};
use crate::volume_header::VolumeHeader;
use crate::volume_set::VolumeSet;

/// Engine `DISK_SECTOR_NPAGES`.
pub const SECTOR_NPAGES: i32 = 64;

/// Engine `SECTOR_FROM_PAGEID`.
pub fn sector_of(pageid: i32) -> i32 {
    pageid / SECTOR_NPAGES
}

/// Sectors one STAB page covers: `DB_PAGESIZE * 8` (130752 for 16 KiB).
pub fn sectors_per_page(io_pagesize: usize) -> i32 {
    (db_pagesize(io_pagesize) * 8) as i32
}

/// Where sector `sectid`'s bit lives: (STAB page, byte offset of its u64 unit in the IO page, bit in the unit).
pub fn bit_position(vh: &VolumeHeader, io_pagesize: usize, sectid: i32) -> (Vpid, usize, u32) {
    let per_page = sectors_per_page(io_pagesize);
    let page = vh.stab_first_page + sectid / per_page;
    let in_page = sectid % per_page;
    (
        Vpid::new(vh.volid, page),
        IO_RESERVED_SIZE + (in_page / 64) as usize * 8,
        (in_page % 64) as u32,
    )
}

/// The volume's system sectors (header and STAB pages): 0..=`SECTOR_FROM_PAGEID(sys_lastpage)`, always set.
pub fn system_sectors(vh: &VolumeHeader) -> std::ops::RangeInclusive<i32> {
    0..=sector_of(vh.sys_lastpage)
}

/// Every sector of the volume whose bit is set, ascending.
pub fn used_sectors(vols: &VolumeSet, volid: i16) -> io::Result<Vec<i32>> {
    let vh = &vols.volume(volid)?.header;
    let per_page = sectors_per_page(vols.io_pagesize());
    let mut out = Vec::new();
    for p in 0..vh.stab_npages {
        let page = vols.read_page(Vpid::new(volid, vh.stab_first_page + p))?;
        let first = p * per_page;
        let last = (first + per_page).min(vh.nsect_total);
        for s in first..last {
            let unit = get_u64(&page, IO_RESERVED_SIZE + ((s - first) / 64) as usize * 8);
            if unit >> ((s - first) % 64) & 1 != 0 {
                out.push(s);
            }
        }
    }
    Ok(out)
}

/// Reads one sector's bit from its STAB page.
pub fn is_set(stab_page: &[u8], unit_off: usize, bit: u32) -> bool {
    get_u64(stab_page, unit_off) >> bit & 1 != 0
}

/// Sets or clears one sector's bit in its STAB page; returns the old value.
pub fn set_bit(stab_page: &mut [u8], unit_off: usize, bit: u32, on: bool) -> bool {
    let u = get_u64(stab_page, unit_off);
    let old = u >> bit & 1 != 0;
    put_u64(
        stab_page,
        unit_off,
        if on { u | 1 << bit } else { u & !(1 << bit) },
    );
    old
}
