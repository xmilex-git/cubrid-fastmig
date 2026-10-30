//! Records in a develop-format heap that the source never had: create an empty heap file as the engine's
//! `heap_create` does, insert records first fit, put a new record under an existing OID, delete a record, and end
//! with the develop header statistics and bestspace entries measured on every page.
//!
//! Input: a [`SpaceManager`] over the target and a develop heap (one [`create_heap`] made, or one [`rewrite_heap`]
//! already turned into develop's format). Output: the records written; [`PageStats`] of the whole heap.
//! Source: engine develop @5b1867174 `heap_file.c` `heap_create_internal` (:4786-5016: `FILE_HEAP`, header page =
//! sticky first page, `ANCHORED_DONT_REUSE_SLOTS`, `HEAP_MAX_ALIGN` 4, header record in slot 0, `unfill_space` =
//! `DB_PAGESIZE` × `unfill_factor` 0.10, `num_pages` 1), `heap_create_bestspace` (:3896-4073: bestspace pages linked
//! after the header, `last_vpid` = the last of them); `slotted_page.c` `spage_delete` (a non-MVCC delete, as the
//! root class heap's records are). The catalog regeneration of workspace#504 places system rows and class objects
//! through this; where a record goes (first fit over the pages read) is ours, as in workspace#482 decision 6.
//!
//! [`rewrite_heap`]: crate::heap_rewrite::rewrite_heap

use std::io;

use crate::file_alloc::{SpaceManager, perm_tablespace};
use crate::file_header::{FILE_DESCRIPTORS_SIZE, FileHeader, FileType};
use crate::heap_rewrite::header_rewrite::rewrite_header;
use crate::heap_rewrite::heap_state::{HeapState, PageStats, chain_of, editor, invalid, spage_err};
use crate::heap_rewrite::overflow;
use crate::heap_rewrite::placement::{max_slotted_len, place, release_target};
use crate::heap_rewrite::record_patch::{residue_free_overflow_header, with_header};
use crate::page::{self, PageType, db_area, db_area_mut, db_pagesize};
use crate::record::bestspace_page::{self, DEFAULT_SHARDS, ROOT_SHARDS};
use crate::record::heap_hdr_stats::{BestspaceDir, HeapHdrStats, LineFields};
use crate::record::mvcc_header::RecordHeader;
use crate::spage::{Anchor, RecordType, SlottedPage, SpageBuilder};
use crate::{Hfid, Oid, VersionLine, Vpid};

/// Engine default `unfill_factor`.
const UNFILL_FACTOR: f32 = 0.10;
/// Engine `HEAP_MAX_ALIGN` (`INT_ALIGNMENT`).
const HEAP_MAX_ALIGN: usize = 4;

/// Creates an empty heap file of class `class_oid` (`root`: the root class heap's single bestspace shard). The
/// header record and bestspace entries are written again by [`HeapWriter::finish`].
pub fn create_heap(space: &SpaceManager, class_oid: Oid, root: bool) -> io::Result<Hfid> {
    let vs = space.volumes();
    let io_size = vs.io_pagesize();
    let mut des = [0u8; FILE_DESCRIPTORS_SIZE];
    class_oid.write(&mut des, 0);
    let vfid = space.create_file(FileType::Heap, des, perm_tablespace(io_size, 1))?;
    let hv = space.alloc_sticky_first_page(vfid, PageType::Heap)?;
    let hfid = Hfid {
        vfid,
        pageid: hv.pageid,
    };
    hfid.write(&mut des, 8);
    space.set_descriptor(vfid, des)?;

    let bs = (0..bestspace_page::page_count(io_size))
        .map(|_| space.alloc_page(vfid, PageType::Heap))
        .collect::<io::Result<Vec<_>>>()?;
    let n = bs.len();
    for (i, &v) in bs.iter().enumerate() {
        let mut p = vec![0u8; io_size];
        page::init_header(&mut p, v, PageType::Heap);
        let prev = if i == 0 { hv } else { bs[i - 1] };
        let next = bs.get(i + 1).copied().unwrap_or(Vpid::NULL);
        bestspace_page::write(db_area_mut(&mut p), io_size, i, class_oid, prev, next, &[]);
        vs.write_page(v, &p)?;
    }
    let hdr = HeapHdrStats {
        class_oid,
        ovf_vfid: crate::Vfid::NULL,
        next_vpid: bs[0],
        last_vpid: bs[n - 1],
        unfill_space: (db_pagesize(io_size) as f32 * UNFILL_FACTOR) as i32,
        num_pages: 1,
        num_recs: 0,
        recs_sumlen: 0,
        line: LineFields::Develop(BestspaceDir {
            candidates: Vec::new(),
            num_shards: if root { ROOT_SHARDS } else { DEFAULT_SHARDS },
            pages: bs,
        }),
    };
    let mut rec = vec![0u8; HeapHdrStats::size(VersionLine::Develop)];
    hdr.write(&mut rec, VersionLine::Develop);
    let mut p = vec![0u8; io_size];
    page::init_header(&mut p, hv, PageType::Heap);
    let mut b = SpageBuilder::new(
        db_area_mut(&mut p),
        Anchor::AnchoredDontReuseSlots,
        HEAP_MAX_ALIGN,
        true,
    );
    b.insert(RecordType::Home, &rec)
        .ok_or_else(|| invalid(format!("{hv:?}: the heap header does not fit")))?;
    b.finish();
    vs.write_page(hv, &p)?;
    Ok(hfid)
}

/// An open heap of the target line (develop, or 11.4 on the 11.4 same-version path): every page of its chain read,
/// ready for inserts, rewrites and deletes.
pub struct HeapWriter<'s, 'v> {
    st: HeapState<'s, 'v>,
    hdr: HeapHdrStats,
}

impl<'s, 'v> HeapWriter<'s, 'v> {
    /// Reads the header and every data page of heap `hfid` (in the layout of `space.line()`).
    pub fn open(space: &'s SpaceManager<'v>, hfid: Hfid) -> io::Result<Self> {
        let vs = space.volumes();
        let hv = hfid.vpid();
        let hp = vs.read_page(hv)?;
        let rec0 = SlottedPage::new(db_area(&hp))
            .and_then(|sp| sp.record(0))
            .ok_or_else(|| invalid(format!("heap header {hv:?} has no slot 0")))?
            .1;
        let hdr = HeapHdrStats::read(rec0, space.line())
            .map_err(|e| invalid(format!("heap header {hv:?}: {e}")))?;
        let reuse_slots = FileHeader::read(&vs.read_page(hfid.vfid.header_vpid())?).file_type()
            == Some(FileType::HeapReuseSlots);
        let mut st = HeapState::new(
            space,
            hfid,
            hdr.class_oid,
            reuse_slots,
            hdr.ovf_vfid,
            hdr.last_vpid,
        );
        st.load(hv)?;
        // the first data page: after the bestspace pages (develop), right after the header (11.4)
        let mut v = match &hdr.line {
            LineFields::Develop(d) => {
                st.bestspace_pages = d.pages.clone();
                let last_bs = *d.pages.last().ok_or_else(|| {
                    invalid(format!("{hv:?}: develop header without bestspace pages"))
                })?;
                st.peek(last_bs, |p| chain_of(p, last_bs))?.next_vpid
            }
            LineFields::V11_4(_) => hdr.next_vpid,
        };
        let limit: u64 = vs
            .volumes()
            .iter()
            .map(|v| v.header.nsect_total.max(0) as u64 * v.header.sect_npgs.max(0) as u64)
            .sum();
        let mut walked = 0u64;
        while !v.is_null() {
            walked += 1;
            if walked > limit {
                return Err(invalid(format!(
                    "heap {hv:?}: chain longer than the volume set"
                )));
            }
            st.load(v)?;
            v = st.peek(v, |p| chain_of(p, v))?.next_vpid;
        }
        st.flush()?;
        Ok(HeapWriter { st, hdr })
    }

    pub fn hfid(&self) -> Hfid {
        self.st.hfid
    }

    /// Inserts `rec` (a whole record as stored in a home slot) in a new slot; `mvcc` says the class is MVCC, which
    /// decides the overflow header of a record too long for a slot. Returns the new OID.
    pub fn insert(&mut self, rec: &[u8], mvcc: bool) -> io::Result<Oid> {
        if rec.len() <= max_slotted_len(self.st.db_pagesize()) {
            return self.st.insert_first_fit(RecordType::Home, rec, Vpid::NULL);
        }
        let big = if mvcc {
            let h = RecordHeader::read(rec).map_err(|e| invalid(format!("new record: {e}")))?;
            with_header(rec, &h, &residue_free_overflow_header(&h))
        } else {
            rec.to_vec()
        };
        if self.st.ovf_vfid.is_null() {
            self.st.ovf_vfid =
                overflow::create_file(self.st.space, self.st.hfid, self.st.class_oid)?;
        }
        let first = overflow::write_chain(self.st.space, self.st.ovf_vfid, &big)?.first;
        self.st.insert_first_fit(
            RecordType::BigOne,
            &overflow::home_pointer(first),
            Vpid::NULL,
        )
    }

    /// Puts `rec` under the existing object `oid` (home slot, relocation or overflow pages, as needed).
    pub fn place(&mut self, oid: Oid, rec: &[u8], mvcc: bool) -> io::Result<()> {
        place(&mut self.st, oid, rec, mvcc).map(|_| ())
    }

    /// Deletes object `oid`: its NEWHOME slot or overflow pages are released, its home slot deleted.
    pub fn delete(&mut self, oid: Oid) -> io::Result<()> {
        release_target(&mut self.st, oid)?;
        let vpid = oid.vpid();
        self.st.edit(vpid, |p| {
            editor(p, vpid)?
                .delete(oid.slotid as usize)
                .map_err(|e| spage_err(vpid, e))
        })
    }

    /// Writes the header record (statistics measured on every page) and the bestspace entries; returns the
    /// statistics.
    pub fn finish(mut self) -> io::Result<PageStats> {
        self.st.flush()?;
        // a develop header keeps its shard count; `root` only picks it for an 11.4 header
        let line = self.st.space.line();
        let out = rewrite_header(&mut self.st, &self.hdr, true, false, line)?;
        self.st.flush()?;
        Ok(out.stats)
    }
}
