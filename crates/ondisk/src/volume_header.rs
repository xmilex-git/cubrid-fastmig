//! The volume header (page 0 of every volume): geometry, creation times, checkpoint LSA, boot HFID and the
//! volume path chain; read, patch and path rewrite.
//!
//! Input: page 0 of a volume (whole IO page). Output: [`VolumeHeader`], or the page with fields rewritten.
//! Source: engine `src/storage/disk_manager.c:74-111` (`DISK_VOLUME_HEADER`), `disk_vhdr_set_vol_fullname` /
//! `disk_vhdr_set_next_vol_fullname` (:5328-5355); identical in release/11.4 @46212adcd and develop @5b1867174
//! (R1 § 2.3, R5 § 2.2). Offsets checked on T1 volume 0 of fm114_clean (workspace#490).
//! 10.2..11.3 write the 128 B layout: no `vol_creation`, every field from `chkpt_lsa` on and `var_fields` 8 B lower;
//! the three var offsets are relative to `var_fields` and keep their values (disk_manager.c:74@release/11.3 @8acbed10e,
//! R8 § 2.3; checked on a createdb volume 0 of 10.2.18, 11.0.16, 11.2.9 and 11.3.6, workspace#522).
//! [`VolumeHeader::read_layout`] decodes either layout.

use std::io;
use std::mem::offset_of;

use zerocopy::little_endian::{I16, I32, I64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use crate::ids::{FileAndPageLe, Hfid};
use crate::lsa::{Lsa, LsaLe};
use crate::page::{IO_RESERVED_SIZE, IO_TRAILER_SIZE};
use crate::source_version::HeaderLayout;

/// Engine magic `CUBRID_MAGIC_DATABASE_VOLUME`.
pub const MAGIC: &str = "CUBRID/Volume";

/// Engine `DISK_VOLUME_HEADER` up to `var_fields`, at the start of the DB page; same in both versions. Size 132 B,
/// followed by `var_fields` (three NUL-terminated strings addressed by the `offset_to_*` fields).
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
pub struct VolumeHeaderLe {
    /// @0, `CUBRID_MAGIC_MAX_LENGTH` = 25, NUL-padded.
    pub magic: [u8; 25],
    pub pad0: u8,
    /// @26
    pub iopagesize: I16,
    /// @28
    pub volid: I16,
    /// @30
    pub db_charset: i8,
    pub dummy1: u8,
    /// @32
    pub purpose: I32,
    /// @36
    pub vol_type: I32,
    /// @40
    pub sect_npgs: I32,
    /// @44
    pub nsect_total: I32,
    /// @48
    pub nsect_max: I32,
    /// @52
    pub hint_allocsect: I32,
    /// @56
    pub stab_npages: I32,
    /// @60
    pub stab_first_page: I32,
    /// @64
    pub sys_lastpage: I32,
    pub dummy2: I32,
    /// @72
    pub db_creation: I64,
    /// @80
    pub vol_creation: I64,
    /// @88
    pub chkpt_lsa: LsaLe,
    /// @96
    pub boot_hfid: FileAndPageLe,
    /// @108, `reserved0..3`.
    pub reserved: [I32; 4],
    /// @124
    pub next_volid: I16,
    /// @126
    pub offset_to_vol_fullname: I16,
    /// @128
    pub offset_to_next_vol_fullname: I16,
    /// @130
    pub offset_to_vol_remarks: I16,
}

/// Offset of `var_fields` in the DB page.
const VAR_FIELDS: usize = size_of::<VolumeHeaderLe>();
const _: () = assert!(VAR_FIELDS == 132);

fn fixed(d: &[u8]) -> &VolumeHeaderLe {
    VolumeHeaderLe::ref_from_prefix(d)
        .expect("a DB page holds the volume header")
        .0
}

fn fixed_mut(d: &mut [u8]) -> &mut VolumeHeaderLe {
    VolumeHeaderLe::mut_from_prefix(d)
        .expect("a DB page holds the volume header")
        .0
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
    /// `None` in a 10.2..11.3 header (the field does not exist).
    pub vol_creation: Option<i64>,
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
    /// Decodes page 0 of a volume written in `layout` (the source version's, [`crate::SourceVersion::header_layout`]).
    pub fn read_layout(page: &[u8], layout: HeaderLayout) -> io::Result<Self> {
        match layout {
            HeaderLayout::Since11_4 => Self::read(page),
            HeaderLayout::Before11_4 => {
                // the 128 B layout is the 136 B one without vol_creation: open its 8 B again and decode the result
                let gap = IO_RESERVED_SIZE + offset_of!(VolumeHeaderLe, vol_creation);
                let Some((head, tail)) = page.split_at_checked(gap) else {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "volume page 0 too short",
                    ));
                };
                let wide = [head, &[0u8; 8], tail].concat();
                Ok(VolumeHeader {
                    vol_creation: None,
                    ..Self::read(&wide)?
                })
            }
        }
    }

    /// Decodes page 0 of a volume in the 136 B layout (11.4, develop); refuses a page whose magic is not
    /// `CUBRID/Volume`.
    pub fn read(page: &[u8]) -> io::Result<Self> {
        let d = db(page);
        if cstr_at(d, 0) != MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not a CUBRID volume (magic)",
            ));
        }
        let h = fixed(d);
        let var_at = |o: I16| VAR_FIELDS + o.get() as usize;
        Ok(VolumeHeader {
            iopagesize: h.iopagesize.get(),
            volid: h.volid.get(),
            db_charset: h.db_charset,
            purpose: h.purpose.get(),
            vol_type: h.vol_type.get(),
            sect_npgs: h.sect_npgs.get(),
            nsect_total: h.nsect_total.get(),
            nsect_max: h.nsect_max.get(),
            hint_allocsect: h.hint_allocsect.get(),
            stab_npages: h.stab_npages.get(),
            stab_first_page: h.stab_first_page.get(),
            sys_lastpage: h.sys_lastpage.get(),
            db_creation: h.db_creation.get(),
            vol_creation: Some(h.vol_creation.get()),
            chkpt_lsa: h.chkpt_lsa.get(),
            boot_hfid: h.boot_hfid.get(),
            next_volid: h.next_volid.get(),
            vol_fullname: cstr_at(d, var_at(h.offset_to_vol_fullname)),
            next_vol_fullname: cstr_at(d, var_at(h.offset_to_next_vol_fullname)),
            vol_remarks: cstr_at(d, var_at(h.offset_to_vol_remarks)),
        })
    }
}

/// Transforms page 0 of a volume from the 10.2..11.3 layout (128 B) to the 136 B one in place: every field from
/// `chkpt_lsa` on and `var_fields` move 8 B up, and `vol_creation` fills the gap. The var offsets are relative to
/// `var_fields` and keep their values. Refuses a page whose last 8 DB bytes are not zero (the strings would not fit)
/// and checks that the result decodes to the same header. The page LSA is not touched (R8 § 2.3).
pub fn transform_from_before_11_4(page: &mut [u8], vol_creation: i64) -> io::Result<()> {
    let before = VolumeHeader::read_layout(page, HeaderLayout::Before11_4)?;
    let db_end = page.len() - IO_TRAILER_SIZE;
    let gap = IO_RESERVED_SIZE + offset_of!(VolumeHeaderLe, vol_creation);
    if page[db_end - 8..db_end].iter().any(|&b| b != 0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "volume header var_fields reach the end of the page",
        ));
    }
    page.copy_within(gap..db_end - 8, gap + 8);
    fixed_mut(&mut page[IO_RESERVED_SIZE..])
        .vol_creation
        .set(vol_creation);
    let after = VolumeHeader::read(page)?;
    if after
        != (VolumeHeader {
            vol_creation: Some(vol_creation),
            ..before
        })
    {
        return Err(io::Error::other("volume header transform changed a field"));
    }
    Ok(())
}

/// What a new permanent data volume's header holds (engine `disk_format`, workspace#500).
#[derive(Clone, Debug)]
pub struct NewVolume<'a> {
    pub volid: i16,
    pub db_charset: i8,
    pub nsect_total: i32,
    pub nsect_max: i32,
    pub stab_npages: i32,
    pub db_creation: i64,
    pub vol_creation: i64,
    pub chkpt_lsa: Lsa,
    pub vol_fullname: &'a str,
    pub vol_remarks: &'a str,
}

/// Engine `disk_format`: the DB page of a new permanent data volume's header — purpose and type permanent,
/// `hint_allocsect` NULL, STAB from page 1, boot HFID NULL, no next volume. The page header (VPID, type, LSA) is the
/// caller's.
pub fn format(page: &mut [u8], v: &NewVolume) -> io::Result<()> {
    let db_end = page.len() - IO_TRAILER_SIZE;
    let d = &mut page[IO_RESERVED_SIZE..db_end];
    d.fill(0);
    let h = fixed_mut(d);
    h.magic[..MAGIC.len()].copy_from_slice(MAGIC.as_bytes());
    h.iopagesize.set((db_end + IO_TRAILER_SIZE) as i16);
    h.volid.set(v.volid);
    h.db_charset = v.db_charset;
    h.purpose.set(0); // DB_PERMANENT_DATA_PURPOSE
    h.vol_type.set(0); // DB_PERMANENT_VOLTYPE
    h.sect_npgs.set(64);
    h.nsect_total.set(v.nsect_total);
    h.nsect_max.set(v.nsect_max);
    h.hint_allocsect.set(-1);
    h.stab_npages.set(v.stab_npages);
    h.stab_first_page.set(1);
    h.sys_lastpage.set(v.stab_npages);
    h.db_creation.set(v.db_creation);
    h.vol_creation.set(v.vol_creation);
    h.chkpt_lsa.set(v.chkpt_lsa);
    h.boot_hfid.set(Hfid::NULL);
    h.next_volid.set(-1);
    set_paths_db(d, [v.vol_fullname, "", v.vol_remarks])
}

/// Writes `nsect_total` (engine `disk_volume_expand`).
pub fn set_nsect_total(page: &mut [u8], nsect_total: i32) {
    fixed_mut(&mut page[IO_RESERVED_SIZE..])
        .nsect_total
        .set(nsect_total);
}

/// Engine `disk_set_link`: links the volume to the next one (id and path); its own path and remarks are kept.
pub fn set_link(page: &mut [u8], next_volid: i16, next_vol_fullname: &str) -> io::Result<()> {
    let h = VolumeHeader::read(page)?;
    fixed_mut(&mut page[IO_RESERVED_SIZE..])
        .next_volid
        .set(next_volid);
    set_paths(page, &h.vol_fullname, next_vol_fullname)
}

/// Writes `db_creation` (must equal the new log header's, R5 L4).
pub fn set_db_creation(page: &mut [u8], v: i64) {
    fixed_mut(&mut page[IO_RESERVED_SIZE..]).db_creation.set(v);
}

/// Writes `chkpt_lsa` (the new log's checkpoint LSA, R5 L4).
pub fn set_chkpt_lsa(page: &mut [u8], lsa: Lsa) {
    fixed_mut(&mut page[IO_RESERVED_SIZE..]).chkpt_lsa.set(lsa);
}

/// Rewrites the volume's own path and the next volume's path, keeping the remarks: `var_fields` becomes
/// `vol_fullname\0 next_vol_fullname\0 remarks\0` and the three offsets are recomputed, like the engine's
/// `disk_vhdr_set_*`. Bytes after the new strings are zeroed. Refuses strings that do not fit in the DB page.
/// The target volume header carries target paths (G1, workspace#481).
pub fn set_paths(page: &mut [u8], vol_fullname: &str, next_vol_fullname: &str) -> io::Result<()> {
    let remarks = VolumeHeader::read(page)?.vol_remarks;
    let db_end = page.len() - IO_TRAILER_SIZE;
    set_paths_db(
        &mut page[IO_RESERVED_SIZE..db_end],
        [vol_fullname, next_vol_fullname, remarks.as_str()],
    )
}

/// Writes the three variable fields into the DB page `d` and their offsets (engine `disk_vhdr_set_*`).
fn set_paths_db(d: &mut [u8], fields: [&str; 3]) -> io::Result<()> {
    let total: usize = fields.iter().map(|s| s.len() + 1).sum();
    if VAR_FIELDS + total > d.len() || fields.iter().any(|s| s.as_bytes().contains(&0)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "volume header paths do not fit in the page",
        ));
    }
    let (h, var) = VolumeHeaderLe::mut_from_prefix(d).expect("checked above");
    var.fill(0);
    let mut at = 0usize;
    for (s, off_field) in fields.iter().zip([
        &mut h.offset_to_vol_fullname,
        &mut h.offset_to_next_vol_fullname,
        &mut h.offset_to_vol_remarks,
    ]) {
        var[at..at + s.len()].copy_from_slice(s.as_bytes());
        off_field.set(at as i16);
        at += s.len() + 1;
    }
    Ok(())
}
