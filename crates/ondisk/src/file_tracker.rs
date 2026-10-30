//! The file tracker: the sorted list of every permanent file of the database (the tracker itself excluded).
//!
//! Input: the volume set and the tracker VFID (`BOOT_DB_PARM::trk_vfid`). Output: the tracked files in order;
//! the tracker page with an item inserted or removed.
//! Source: engine `src/storage/file_manager.c:490-499` (`FILE_TRACK_ITEM`), `file_tracker_init_page`
//! (:10003-10014, extdata over the whole sticky first page, item 16 B), `file_compare_track_items`
//! (:2983-2997, order (volid, fileid)), `file_tracker_register/unregister`; same in release/11.4 @46212adcd and
//! develop @5b1867174 (R5 § 2.5).

use std::io;

use crate::extdata::{self, ExtdataHeader};
use crate::file_header::{FileHeader, FileType};
use crate::ids::{Vfid, Vpid};
use zerocopy::little_endian::{I16, I32, U64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use crate::volume_set::VolumeSet;

/// Engine `FILE_TRACK_ITEM` as stored; the VFID's padding holds the file type.
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct TrackItemLe {
    fileid: I32,
    volid: I16,
    file_type: I16,
    /// heap: byte 0 `is_marked_deleted`.
    metadata: U64,
}

/// `sizeof (FILE_TRACK_ITEM)`.
pub const TRACK_ITEM_SIZE: usize = size_of::<TrackItemLe>();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrackItem {
    pub vfid: Vfid,
    pub file_type_raw: i16,
    pub metadata: u64,
}

impl TrackItem {
    pub fn decode(b: &[u8]) -> Self {
        let t = TrackItemLe::ref_from_prefix(b).expect("a tracker item").0;
        TrackItem {
            vfid: Vfid::new(t.volid.get(), t.fileid.get()),
            file_type_raw: t.file_type.get(),
            metadata: t.metadata.get(),
        }
    }

    pub fn encode(&self) -> [u8; TRACK_ITEM_SIZE] {
        let t = TrackItemLe {
            fileid: self.vfid.fileid.into(),
            volid: self.vfid.volid.into(),
            file_type: self.file_type_raw.into(),
            metadata: self.metadata.into(),
        };
        zerocopy::transmute!(t)
    }

    pub fn file_type(&self) -> Option<FileType> {
        FileType::from_i32(self.file_type_raw as i32)
    }

    /// A heap file waiting for reuse: DROP of a class without OID reuse marks its `FILE_HEAP` deleted in the tracker
    /// (`FILE_TRACK_HEAP_METADATA.is_marked_deleted`, metadata byte 0) instead of destroying it, and the next heap
    /// created takes it (engine `heap_file.c:xheap_destroy_newly_created`, `file_manager.c:file_tracker_reuse_heap`;
    /// release/10.2, release/11.4 and develop alike, workspace#555).
    pub fn is_marked_deleted(&self) -> bool {
        self.file_type() == Some(FileType::Heap) && self.metadata & 0xff != 0
    }
}

/// The tracker's first page (its sticky page), where the item chain starts at offset 0.
pub fn first_page(vols: &VolumeSet, trk_vfid: Vfid) -> io::Result<Vpid> {
    let h = FileHeader::read(&vols.read_page(trk_vfid.header_vpid())?);
    if h.file_type() != Some(FileType::Tracker) || h.vpid_sticky_first.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not a file tracker with a sticky first page",
        ));
    }
    Ok(h.vpid_sticky_first)
}

/// Every tracked file, in tracker order, and the pages holding the items.
pub fn read(vols: &VolumeSet, trk_vfid: Vfid) -> io::Result<(Vec<TrackItem>, Vec<Vpid>)> {
    let mut items = Vec::new();
    let mut pages = Vec::new();
    for (c, page) in extdata::chain(vols, first_page(vols, trk_vfid)?, 0, 1 << 20)? {
        pages.push(c.vpid);
        for i in 0..ExtdataHeader::read(&page, 0).n_items as usize {
            items.push(TrackItem::decode(extdata::item(&page, 0, i)));
        }
    }
    Ok((items, pages))
}
