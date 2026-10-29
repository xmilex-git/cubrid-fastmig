//! Object identifiers that name a page, file, sector, record or index on disk: VPID, VFID, VSID, OID, HFID, BTID, EHID.
//!
//! Input: the 8- or 12-byte encoded identifier at an offset. Output: the identifier, or the bytes written back.
//! Source: engine `src/storage/storage_common.h` (`struct vpid`, `vfid`, `vsid`, `hfid`, `btid`, `ehid`) and
//! `src/compat/dbtype_def.h` (`struct db_identifier` = OID); same layout in release/11.4 @46212adcd and develop @5b1867174.

use zerocopy::little_endian::{I16, I32};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use crate::le_field::{get_i16, get_i32, put_i16, put_i32};

/// Engine `NULL_PAGEID` / `NULL_FILEID` / `NULL_SECTID`.
pub const NULL_ID: i32 = -1;
/// Engine `NULL_VOLID`.
pub const NULL_VOLID: i16 = -1;
/// Engine `NULL_SLOTID`.
pub const NULL_SLOTID: i16 = -1;

/// Engine `VPID`: one page of one volume. Size 8 B: `pageid` i32 @0, `volid` i16 @4, 2 B padding @6.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Vpid {
    /// Volume first so that the derived order is (volid, pageid), the order of pages in a volume set.
    pub volid: i16,
    pub pageid: i32,
}

impl Vpid {
    pub const SIZE: usize = 8;
    pub const NULL: Vpid = Vpid {
        volid: NULL_VOLID,
        pageid: NULL_ID,
    };

    pub fn new(volid: i16, pageid: i32) -> Self {
        Vpid { volid, pageid }
    }

    pub fn is_null(&self) -> bool {
        self.pageid == NULL_ID
    }

    pub fn read(buf: &[u8], off: usize) -> Self {
        Vpid {
            pageid: get_i32(buf, off),
            volid: get_i16(buf, off + 4),
        }
    }

    /// Writes the 8 bytes, padding zeroed.
    pub fn write(&self, buf: &mut [u8], off: usize) {
        put_i32(buf, off, self.pageid);
        put_i16(buf, off + 4, self.volid);
        put_i16(buf, off + 6, 0);
    }
}

/// Engine `VFID`: a file, named by the page id of its header page. Size 8 B: `fileid` i32 @0, `volid` i16 @4, pad @6.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Vfid {
    pub volid: i16,
    pub fileid: i32,
}

impl Vfid {
    pub const SIZE: usize = 8;
    pub const NULL: Vfid = Vfid {
        volid: NULL_VOLID,
        fileid: NULL_ID,
    };

    pub fn new(volid: i16, fileid: i32) -> Self {
        Vfid { volid, fileid }
    }

    pub fn is_null(&self) -> bool {
        self.fileid == NULL_ID
    }

    /// The file header page (engine `FILE_GET_HEADER_VPID`).
    pub fn header_vpid(&self) -> Vpid {
        Vpid::new(self.volid, self.fileid)
    }

    pub fn read(buf: &[u8], off: usize) -> Self {
        Vfid {
            fileid: get_i32(buf, off),
            volid: get_i16(buf, off + 4),
        }
    }

    pub fn write(&self, buf: &mut [u8], off: usize) {
        put_i32(buf, off, self.fileid);
        put_i16(buf, off + 4, self.volid);
        put_i16(buf, off + 6, 0);
    }
}

/// Engine `VSID`: one sector (64 pages) of a volume. Size 8 B: `sectid` i32 @0, `volid` i16 @4, pad @6.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Vsid {
    pub volid: i16,
    pub sectid: i32,
}

impl Vsid {
    pub const SIZE: usize = 8;

    pub fn new(volid: i16, sectid: i32) -> Self {
        Vsid { volid, sectid }
    }

    pub fn read(buf: &[u8], off: usize) -> Self {
        Vsid {
            sectid: get_i32(buf, off),
            volid: get_i16(buf, off + 4),
        }
    }

    pub fn write(&self, buf: &mut [u8], off: usize) {
        put_i32(buf, off, self.sectid);
        put_i16(buf, off + 4, self.volid);
        put_i16(buf, off + 6, 0);
    }
}

/// Engine `OID`: a record slot. Size 8 B: `pageid` i32 @0, `slotid` i16 @4, `volid` i16 @6.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Oid {
    /// Field order gives the derived order (volid, pageid, slotid), the engine's `oid_compare` order.
    pub volid: i16,
    pub pageid: i32,
    pub slotid: i16,
}

impl Oid {
    pub const SIZE: usize = 8;
    pub const NULL: Oid = Oid {
        volid: NULL_VOLID,
        pageid: NULL_ID,
        slotid: NULL_SLOTID,
    };

    pub fn new(volid: i16, pageid: i32, slotid: i16) -> Self {
        Oid {
            volid,
            pageid,
            slotid,
        }
    }

    pub fn is_null(&self) -> bool {
        self.pageid == NULL_ID
    }

    pub fn vpid(&self) -> Vpid {
        Vpid::new(self.volid, self.pageid)
    }

    pub fn read(buf: &[u8], off: usize) -> Self {
        Oid {
            pageid: get_i32(buf, off),
            slotid: get_i16(buf, off + 4),
            volid: get_i16(buf, off + 6),
        }
    }

    pub fn write(&self, buf: &mut [u8], off: usize) {
        put_i32(buf, off, self.pageid);
        put_i16(buf, off + 4, self.slotid);
        put_i16(buf, off + 6, self.volid);
    }
}

/// A VFID followed by one page id: engine `HFID` (`hpgid` = heap header page), `BTID` (`root_pageid`) and
/// `EHID` (`pageid` = directory root). Size 12 B: VFID @0, page id i32 @8.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FileAndPage {
    pub vfid: Vfid,
    pub pageid: i32,
}

/// Engine `HFID`.
pub type Hfid = FileAndPage;
/// Engine `BTID`.
pub type Btid = FileAndPage;
/// Engine `EHID`.
pub type Ehid = FileAndPage;

impl FileAndPage {
    pub const SIZE: usize = 12;
    pub const NULL: FileAndPage = FileAndPage {
        vfid: Vfid::NULL,
        pageid: NULL_ID,
    };

    pub fn is_null(&self) -> bool {
        self.vfid.is_null()
    }

    /// The page named by `pageid`, on the file's volume.
    pub fn vpid(&self) -> Vpid {
        Vpid::new(self.vfid.volid, self.pageid)
    }

    pub fn read(buf: &[u8], off: usize) -> Self {
        FileAndPage {
            vfid: Vfid::read(buf, off),
            pageid: get_i32(buf, off + 8),
        }
    }

    pub fn write(&self, buf: &mut [u8], off: usize) {
        self.vfid.write(buf, off);
        put_i32(buf, off + 8, self.pageid);
    }
}

/// Engine `VPID` as it sits inside a fixed on-disk struct: `pageid` @0, `volid` @4, 2 B padding @6 (zero).
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned, Clone, Copy)]
#[repr(C)]
pub struct VpidLe {
    pageid: I32,
    volid: I16,
    pad: [u8; 2],
}

impl VpidLe {
    pub fn get(&self) -> Vpid {
        Vpid::new(self.volid.get(), self.pageid.get())
    }

    /// Writes the identifier, padding zeroed.
    pub fn set(&mut self, v: Vpid) {
        *self = v.into();
    }
}

impl From<Vpid> for VpidLe {
    fn from(v: Vpid) -> Self {
        VpidLe {
            pageid: v.pageid.into(),
            volid: v.volid.into(),
            pad: [0; 2],
        }
    }
}

/// Engine `VFID` as it sits inside a fixed on-disk struct: `fileid` @0, `volid` @4, 2 B padding @6 (zero).
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned, Clone, Copy)]
#[repr(C)]
pub struct VfidLe {
    fileid: I32,
    volid: I16,
    pad: [u8; 2],
}

impl VfidLe {
    pub fn get(&self) -> Vfid {
        Vfid::new(self.volid.get(), self.fileid.get())
    }

    /// Writes the identifier, padding zeroed.
    pub fn set(&mut self, v: Vfid) {
        *self = v.into();
    }
}

impl From<Vfid> for VfidLe {
    fn from(v: Vfid) -> Self {
        VfidLe {
            fileid: v.fileid.into(),
            volid: v.volid.into(),
            pad: [0; 2],
        }
    }
}

/// Engine `OID` as it sits inside a fixed on-disk struct: `pageid` @0, `slotid` @4, `volid` @6.
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned, Clone, Copy)]
#[repr(C)]
pub struct OidLe {
    pageid: I32,
    slotid: I16,
    volid: I16,
}

impl OidLe {
    pub fn get(&self) -> Oid {
        Oid::new(self.volid.get(), self.pageid.get(), self.slotid.get())
    }

    pub fn set(&mut self, o: Oid) {
        *self = o.into();
    }
}

impl From<Oid> for OidLe {
    fn from(o: Oid) -> Self {
        OidLe {
            pageid: o.pageid.into(),
            slotid: o.slotid.into(),
            volid: o.volid.into(),
        }
    }
}

/// Engine `HFID` / `BTID` / `EHID` as it sits inside a fixed on-disk struct: VFID @0, page id @8.
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned, Clone, Copy)]
#[repr(C)]
pub struct FileAndPageLe {
    vfid: VfidLe,
    pageid: I32,
}

impl FileAndPageLe {
    pub fn get(&self) -> FileAndPage {
        FileAndPage {
            vfid: self.vfid.get(),
            pageid: self.pageid.get(),
        }
    }

    pub fn set(&mut self, f: FileAndPage) {
        self.vfid.set(f.vfid);
        self.pageid.set(f.pageid);
    }
}

const _: () = assert!(size_of::<VpidLe>() == Vpid::SIZE && size_of::<VfidLe>() == Vfid::SIZE);
const _: () =
    assert!(size_of::<OidLe>() == Oid::SIZE && size_of::<FileAndPageLe>() == FileAndPage::SIZE);

/// Engine `VSID` as it sits inside a fixed on-disk struct: `sectid` @0, `volid` @4, 2 B padding @6 (zero).
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned, Clone, Copy)]
#[repr(C)]
pub struct VsidLe {
    sectid: I32,
    volid: I16,
    pad: [u8; 2],
}

impl VsidLe {
    pub fn get(&self) -> Vsid {
        Vsid::new(self.volid.get(), self.sectid.get())
    }
}

impl From<Vsid> for VsidLe {
    fn from(v: Vsid) -> Self {
        VsidLe {
            sectid: v.sectid.into(),
            volid: v.volid.into(),
            pad: [0; 2],
        }
    }
}
const _: () = assert!(size_of::<VsidLe>() == Vsid::SIZE);
