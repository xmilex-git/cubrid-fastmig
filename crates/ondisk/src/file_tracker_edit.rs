//! Registering and unregistering a file in the file tracker when the tracker spans several pages.
//!
//! Input: a [`SpaceOp`], the tracker VFID and the item. Output: the item inserted at its ordered place in the
//! first tracker page with room (a new tracker page, allocated as a user page of the tracker file and linked at
//! the end, when every page is full), or removed with the page merged into its neighbour and deallocated when the
//! two fit in one. Each tracker page is ordered by (volid, fileid); the chain as a whole is not.
//! Source: engine `src/storage/file_manager.c` `file_tracker_register_internal` (:10072-10170),
//! `file_tracker_unregister` (:10179-10242), `file_tracker_init_page` (:10003-10014), `file_compare_track_items`;
//! develop 682b080b2, same in release/11.4.

use std::io;

use crate::extdata::{self, Component};
use crate::file_table_edit::{AllocType, SpaceOp};
use crate::file_tracker::{TRACK_ITEM_SIZE, TrackItem};
use crate::ids::{Vfid, Vpid};
use crate::page::{self, PageType};

/// Engine `file_compare_track_items` as a key.
fn track_key(item: &[u8]) -> (i16, i32) {
    let t = TrackItem::decode(item);
    (t.vfid.volid, t.vfid.fileid)
}

impl SpaceOp<'_, '_> {
    /// The tracker's first page (its sticky first page), where the item chain starts at offset 0.
    fn tracker_head(&mut self, trk_vfid: Vfid) -> io::Result<Vpid> {
        let v = self.header(trk_vfid)?.vpid_sticky_first;
        if v.is_null() {
            return Err(io::Error::other("file tracker has no sticky first page"));
        }
        Ok(v)
    }

    /// Engine `file_tracker_register_internal`.
    pub fn tracker_register(&mut self, trk_vfid: Vfid, item: TrackItem) -> io::Result<()> {
        let head = self.tracker_head(trk_vfid)?;
        let c = match self.find_not_full(head, 0)? {
            Ok(c) => c,
            Err(last) => {
                // file_alloc (trk, file_tracker_init_page): a user page of the tracker, linked after the last
                let nv = self.perm_alloc(trk_vfid, AllocType::UserPage)?;
                let np = self.pages.blank(nv);
                page::init_header(np, nv, PageType::Ftab);
                extdata::init_page(np, TRACK_ITEM_SIZE);
                let lp = self.pages.get(last.vpid)?;
                let mut lh = extdata::ExtdataHeader::read(lp, last.at);
                lh.vpid_next = nv;
                lh.write(lp, last.at);
                Component { vpid: nv, at: 0 }
            }
        };
        let want = (item.vfid.volid, item.vfid.fileid);
        let p = self.pages.get(c.vpid)?;
        let pos = extdata::find_ordered(p, c.at, |it| track_key(it).cmp(&want))
            .err()
            .ok_or_else(|| io::Error::other(format!("file {want:?} already tracked")))?;
        let bytes: [u8; TRACK_ITEM_SIZE] = item.encode();
        extdata::insert_at(p, c.at, pos, &bytes);
        Ok(())
    }

    /// Engine `file_tracker_unregister`: returns the removed item.
    pub fn tracker_unregister(&mut self, trk_vfid: Vfid, vfid: Vfid) -> io::Result<TrackItem> {
        let head = self.tracker_head(trk_vfid)?;
        let (removed, merged) =
            self.find_and_remove(head, 0, (vfid.volid, vfid.fileid), track_key)?;
        if let Some(m) = merged {
            // file_dealloc of the merged page (run at commit as file_perm_dealloc of a user page)
            self.perm_dealloc(trk_vfid, m, AllocType::UserPage)?;
        }
        Ok(TrackItem::decode(&removed))
    }
}
