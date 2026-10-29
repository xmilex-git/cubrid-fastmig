//! The reserved header and trailer every volume page carries (engine `FILEIO_PAGE`), and the page size constants.
//!
//! Input: one whole IO page (`io_pagesize` bytes). Output: its LSA, VPID, page type and TDE flag, or those fields
//! written back. The DB page (what slotted pages, file headers and volume headers lay out) starts at
//! [`IO_RESERVED_SIZE`] and is [`db_pagesize`] bytes long.
//! Source: engine `src/storage/file_io.h:165-234` (`fileio_page_reserved`, `fileio_page_watermark`,
//! `fileio_is_page_sane`), `src/storage/storage_common.h:149-166` (`PAGE_TYPE`), `storage_common.c:44-74`;
//! same in release/11.4 @46212adcd and develop @5b1867174; R1 § 2.2. Offsets checked on T1 pages (workspace#490).

use crate::ids::Vpid;
use zerocopy::little_endian::{I16, I32, U64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use crate::lsa::{Lsa, LsaLe};

/// Default `IO_PAGESIZE` (createdb default, and every T1 baseline DB). A database's real page size is read from its
/// volume header at run time ([`crate::volume_set::VolumeSet::io_pagesize`]); 4 KiB and 8 KiB also exist.
pub const IO_PAGESIZE_DEFAULT: usize = 16384;
/// Engine `sizeof (FILEIO_PAGE_RESERVED)`: header bytes before the DB page.
pub const IO_RESERVED_SIZE: usize = 32;
/// Engine `sizeof (FILEIO_PAGE_WATERMARK)`: trailer bytes after the DB page (a copy of the header LSA).
pub const IO_TRAILER_SIZE: usize = 8;

/// Engine `DB_PAGESIZE` for an IO page size: 16344 for 16 KiB.
pub const fn db_pagesize(io_pagesize: usize) -> usize {
    io_pagesize - IO_RESERVED_SIZE - IO_TRAILER_SIZE
}

/// Engine `FILEIO_PAGE_RESERVED`, 32 B at page offset 0, same in both versions. The watermark `prv2.lsa` (a copy of
/// `lsa`) sits at `io_pagesize - 8`.
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
pub struct PageReserved {
    /// @0
    pub lsa: LsaLe,
    /// @8
    pub pageid: I32,
    /// @12
    pub volid: I16,
    /// @14, [`PageType`] as stored.
    pub ptype: u8,
    /// @15, TDE flags ([`PFLAG_TDE_MASK`]).
    pub pflag: u8,
    /// @16
    pub p_reserve_1: I32,
    /// @20
    pub p_reserve_2: I32,
    /// @24
    pub tde_nonce: U64,
}
const _: () = assert!(size_of::<PageReserved>() == IO_RESERVED_SIZE);

/// The reserved header of a whole IO page.
pub fn reserved(page: &[u8]) -> &PageReserved {
    PageReserved::ref_from_prefix(page)
        .expect("an IO page holds its header")
        .0
}

pub fn reserved_mut(page: &mut [u8]) -> &mut PageReserved {
    PageReserved::mut_from_prefix(page)
        .expect("an IO page holds its header")
        .0
}

/// Engine `PAGE_TYPE` (u8 at page offset 14). Values are the same in 11.4 and develop.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum PageType {
    Unknown = 0,
    Ftab = 1,
    Heap = 2,
    VolHeader = 3,
    VolBitmap = 4,
    QResult = 5,
    Ehash = 6,
    Overflow = 7,
    Area = 8,
    Catalog = 9,
    Btree = 10,
    Log = 11,
    DroppedFiles = 12,
    VacuumData = 13,
}

impl PageType {
    pub fn from_u8(v: u8) -> Option<Self> {
        use PageType::*;
        Some(match v {
            0 => Unknown,
            1 => Ftab,
            2 => Heap,
            3 => VolHeader,
            4 => VolBitmap,
            5 => QResult,
            6 => Ehash,
            7 => Overflow,
            8 => Area,
            9 => Catalog,
            10 => Btree,
            11 => Log,
            12 => DroppedFiles,
            13 => VacuumData,
            _ => return None,
        })
    }
}

/// Engine `FILEIO_PAGE_FLAG_ENCRYPTED_AES | _ARIA`: a TDE-encrypted page (R1 § 3 R0 refuses such sources).
pub const PFLAG_TDE_MASK: u8 = 0x3;

pub fn lsa(page: &[u8]) -> Lsa {
    reserved(page).lsa.get()
}

pub fn watermark(page: &[u8]) -> Lsa {
    Lsa::read(page, page.len() - IO_TRAILER_SIZE)
}

/// Engine `fileio_is_page_sane`: the header LSA equals the trailer copy. There is no checksum on data pages.
pub fn is_sane(page: &[u8]) -> bool {
    page[0..8] == page[page.len() - IO_TRAILER_SIZE..]
}

/// The VPID the page header claims; equals the page's position for every page the engine has written.
pub fn vpid(page: &[u8]) -> Vpid {
    let r = reserved(page);
    Vpid::new(r.volid.get(), r.pageid.get())
}

/// The raw page type byte; [`PageType::from_u8`] names it.
pub fn ptype_raw(page: &[u8]) -> u8 {
    reserved(page).ptype
}

pub fn ptype(page: &[u8]) -> Option<PageType> {
    PageType::from_u8(reserved(page).ptype)
}

pub fn pflag(page: &[u8]) -> u8 {
    reserved(page).pflag
}

/// Writes the LSA in both the header and the trailer, keeping the page sane.
pub fn set_lsa(page: &mut [u8], lsa: Lsa) {
    let n = page.len();
    reserved_mut(page).lsa.set(lsa);
    lsa.write(page, n - IO_TRAILER_SIZE);
}

pub fn set_ptype(page: &mut [u8], ptype: PageType) {
    reserved_mut(page).ptype = ptype as u8;
}

/// Header of a page this tool allocates: given VPID and type, NULL LSA (G2, workspace#482), no TDE flag, reserved
/// fields zero. The DB page area is left as is.
pub fn init_header(page: &mut [u8], vpid: Vpid, ptype: PageType) {
    let r = reserved_mut(page);
    r.as_mut_bytes().fill(0);
    r.pageid.set(vpid.pageid);
    r.volid.set(vpid.volid);
    r.ptype = ptype as u8;
    set_lsa(page, Lsa::NULL);
}

/// The DB page area (what the engine's `PAGE_PTR` points at).
pub fn db_area(page: &[u8]) -> &[u8] {
    &page[IO_RESERVED_SIZE..page.len() - IO_TRAILER_SIZE]
}

pub fn db_area_mut(page: &mut [u8]) -> &mut [u8] {
    let n = page.len();
    &mut page[IO_RESERVED_SIZE..n - IO_TRAILER_SIZE]
}
