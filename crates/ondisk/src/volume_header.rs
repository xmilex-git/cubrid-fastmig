//! The volume header (page 0 of every volume): geometry, creation times, checkpoint LSA, boot HFID and the
//! volume path chain; read, patch and path rewrite.
//!
//! Input: page 0 of a volume (whole IO page). Output: [`VolumeHeader`], or the page with fields rewritten.
//! Source: engine `src/storage/disk_manager.c:74-111` (`DISK_VOLUME_HEADER`), `disk_vhdr_set_vol_fullname` /
//! `disk_vhdr_set_next_vol_fullname` (:5328-5355); identical in release/11.4 @46212adcd and develop @5b1867174
//! (R1 § 2.3, R5 § 2.2). Offsets checked on T1 volume 0 of fm114_clean (workspace#490).

use std::io;

use crate::ids::Hfid;
use crate::le_field::{get_i16, get_i32, get_i64, put_i16, put_i64};
use crate::lsa::Lsa;
use crate::page::{IO_RESERVED_SIZE, IO_TRAILER_SIZE};

/// Engine magic `CUBRID_MAGIC_DATABASE_VOLUME`.
pub const MAGIC: &str = "CUBRID/Volume";

/// Field offsets from the start of the DB page (add [`IO_RESERVED_SIZE`] for the IO page), both versions.
pub mod off {
    pub const MAGIC: usize = 0; // char[25]
    pub const IOPAGESIZE: usize = 26;
    pub const VOLID: usize = 28;
    pub const DB_CHARSET: usize = 30;
    pub const PURPOSE: usize = 32;
    pub const TYPE: usize = 36;
    pub const SECT_NPGS: usize = 40;
    pub const NSECT_TOTAL: usize = 44;
    pub const NSECT_MAX: usize = 48;
    pub const HINT_ALLOCSECT: usize = 52;
    pub const STAB_NPAGES: usize = 56;
    pub const STAB_FIRST_PAGE: usize = 60;
    pub const SYS_LASTPAGE: usize = 64;
    pub const DB_CREATION: usize = 72;
    pub const VOL_CREATION: usize = 80;
    pub const CHKPT_LSA: usize = 88;
    pub const BOOT_HFID: usize = 96;
    pub const NEXT_VOLID: usize = 124;
    pub const OFFSET_TO_VOL_FULLNAME: usize = 126;
    pub const OFFSET_TO_NEXT_VOL_FULLNAME: usize = 128;
    pub const OFFSET_TO_VOL_REMARKS: usize = 130;
    pub const VAR_FIELDS: usize = 132;
}

/// Engine `DISK_VOLUME_HEADER`, decoded. Size: 132 B fixed + `var_fields` (three NUL-terminated strings).
#[derive(Clone, Debug, PartialEq)]
pub struct VolumeHeader {
    pub iopagesize: i16,
    pub volid: i16,
    pub db_charset: i8,
    /// `DB_VOLPURPOSE`: 0 = permanent data.
    pub purpose: i32,
    /// `DB_VOLTYPE`: 0 = permanent.
    pub vol_type: i32,
    /// Pages per sector, always 64 (`DISK_SECTOR_NPAGES`).
    pub sect_npgs: i32,
    pub nsect_total: i32,
    pub nsect_max: i32,
    pub hint_allocsect: i32,
    pub stab_npages: i32,
    pub stab_first_page: i32,
    pub sys_lastpage: i32,
    pub db_creation: i64,
    pub vol_creation: i64,
    pub chkpt_lsa: Lsa,
    /// The boot parameter heap; meaningful on volume 0 only.
    pub boot_hfid: Hfid,
    /// Next permanent volume, -1 on the last.
    pub next_volid: i16,
    pub vol_fullname: String,
    pub next_vol_fullname: String,
    pub vol_remarks: String,
}

fn db(page: &[u8]) -> &[u8] {
    &page[IO_RESERVED_SIZE..]
}

fn cstr_at(buf: &[u8], off: usize) -> String {
    let s = &buf[off..];
    let end = s.iter().position(|&b| b == 0).unwrap_or(s.len());
    String::from_utf8_lossy(&s[..end]).into_owned()
}

impl VolumeHeader {
    /// Decodes page 0 of a volume; refuses a page whose magic is not `CUBRID/Volume`.
    pub fn read(page: &[u8]) -> io::Result<Self> {
        let d = db(page);
        if cstr_at(d, off::MAGIC) != MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not a CUBRID volume (magic)",
            ));
        }
        let var = off::VAR_FIELDS;
        let i32_at = |o| get_i32(d, o);
        Ok(VolumeHeader {
            iopagesize: get_i16(d, off::IOPAGESIZE),
            volid: get_i16(d, off::VOLID),
            db_charset: d[off::DB_CHARSET] as i8,
            purpose: i32_at(off::PURPOSE),
            vol_type: i32_at(off::TYPE),
            sect_npgs: i32_at(off::SECT_NPGS),
            nsect_total: i32_at(off::NSECT_TOTAL),
            nsect_max: i32_at(off::NSECT_MAX),
            hint_allocsect: i32_at(off::HINT_ALLOCSECT),
            stab_npages: i32_at(off::STAB_NPAGES),
            stab_first_page: i32_at(off::STAB_FIRST_PAGE),
            sys_lastpage: i32_at(off::SYS_LASTPAGE),
            db_creation: get_i64(d, off::DB_CREATION),
            vol_creation: get_i64(d, off::VOL_CREATION),
            chkpt_lsa: Lsa::read(d, off::CHKPT_LSA),
            boot_hfid: Hfid::read(d, off::BOOT_HFID),
            next_volid: get_i16(d, off::NEXT_VOLID),
            vol_fullname: cstr_at(d, var + get_i16(d, off::OFFSET_TO_VOL_FULLNAME) as usize),
            next_vol_fullname: cstr_at(
                d,
                var + get_i16(d, off::OFFSET_TO_NEXT_VOL_FULLNAME) as usize,
            ),
            vol_remarks: cstr_at(d, var + get_i16(d, off::OFFSET_TO_VOL_REMARKS) as usize),
        })
    }
}

/// Writes `db_creation` (must equal the new log header's, R5 L4).
pub fn set_db_creation(page: &mut [u8], v: i64) {
    put_i64(&mut page[IO_RESERVED_SIZE..], off::DB_CREATION, v);
}

/// Writes `chkpt_lsa` (the new log's checkpoint LSA, R5 L4).
pub fn set_chkpt_lsa(page: &mut [u8], lsa: Lsa) {
    lsa.write(&mut page[IO_RESERVED_SIZE..], off::CHKPT_LSA);
}

/// Rewrites the volume's own path and the next volume's path, keeping the remarks: `var_fields` becomes
/// `vol_fullname\0 next_vol_fullname\0 remarks\0` and the three offsets are recomputed, like the engine's
/// `disk_vhdr_set_*`. Bytes after the new strings are zeroed. Refuses strings that do not fit in the DB page.
/// The target volume header carries target paths (G1, workspace#481).
pub fn set_paths(page: &mut [u8], vol_fullname: &str, next_vol_fullname: &str) -> io::Result<()> {
    let remarks = VolumeHeader::read(page)?.vol_remarks;
    let db_end = page.len() - IO_TRAILER_SIZE;
    let d = &mut page[IO_RESERVED_SIZE..db_end];
    let fields = [vol_fullname, next_vol_fullname, remarks.as_str()];
    let total: usize = fields.iter().map(|s| s.len() + 1).sum();
    if off::VAR_FIELDS + total > d.len() || fields.iter().any(|s| s.as_bytes().contains(&0)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "volume header paths do not fit in the page",
        ));
    }
    let var = &mut d[off::VAR_FIELDS..];
    var.fill(0);
    let mut at = 0usize;
    for (s, off_field) in fields.iter().zip([
        off::OFFSET_TO_VOL_FULLNAME,
        off::OFFSET_TO_NEXT_VOL_FULLNAME,
        off::OFFSET_TO_VOL_REMARKS,
    ]) {
        d[off::VAR_FIELDS + at..off::VAR_FIELDS + at + s.len()].copy_from_slice(s.as_bytes());
        put_i16(d, off_field, at as i16);
        at += s.len() + 1;
    }
    Ok(())
}
