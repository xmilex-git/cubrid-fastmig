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
use crate::le_field::{get_i16, get_i32, get_u64, put_i16, put_i32, put_u64};
use crate::volume_set::VolumeSet;

/// `sizeof (FILE_TRACK_ITEM)`: `fileid` i32 @0, `volid` i16 @4, `type` i16 @6, `metadata` 8 B @8 (heap: byte 0
/// `is_marked_deleted`).
pub const TRACK_ITEM_SIZE: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrackItem {
    pub vfid: Vfid,
    pub file_type_raw: i16,
    pub metadata: u64,
}

impl TrackItem {
    pub fn decode(b: &[u8]) -> Self {
        TrackItem {
            vfid: Vfid::new(get_i16(b, 4), get_i32(b, 0)),
            file_type_raw: get_i16(b, 6),
            metadata: get_u64(b, 8),
        }
    }

    pub fn encode(&self) -> [u8; TRACK_ITEM_SIZE] {
        let mut b = [0u8; TRACK_ITEM_SIZE];
        put_i32(&mut b, 0, self.vfid.fileid);
        put_i16(&mut b, 4, self.vfid.volid);
        put_i16(&mut b, 6, self.file_type_raw);
        put_u64(&mut b, 8, self.metadata);
        b
    }

    pub fn file_type(&self) -> Option<FileType> {
        FileType::from_i32(self.file_type_raw as i32)
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
