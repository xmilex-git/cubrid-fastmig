//! The working state of one heap rewrite: the page being edited, every heap page read so far with its free space
//! and statistics, the chain end, and the overflow file.
//!
//! Input: a [`SpaceManager`] over the target volumes and the heap's identity. Output: page edits written through to
//! the volumes, NEWHOME slots on pages already read (first fit) or on a page appended to the chain, and per-page
//! free space and statistics for the header and bestspace entries. Memory: one page buffer plus [`PageInfo`]
//! (about 40 B) per heap page read.
//! Source: workspace#482 decision 6 (NEWHOME on the first page already read that fits, else a new page at the chain
//! end) and 7 (bestspace and statistics from the pages read); engine `heap_file.c` `heap_vpid_init_new` (new heap
//! page: `ANCHORED_DONT_REUSE_SLOTS`, alignment 4, space saving on, `HEAP_CHAIN` in slot 0 as REC_HOME),
//! `heap_vpid_alloc` / `heap_add_chain_links` (new page linked after `last_vpid`), `slotted_page.c`
//! `spage_collect_statistics` (:996-1045), `spage_max_space_for_new_record` (develop @5b1867174).

use std::collections::HashMap;
use std::io;

use crate::file_alloc::SpaceManager;
use crate::page::{self, PageType, db_area, db_area_mut, db_pagesize};
use crate::record::heap_chain::HeapChain;
use crate::spage::{
    Anchor, RecordType, SPAGE_SLOT_SIZE, SlottedPage, SpageBuilder, SpageEditor, SpageError,
};
use crate::volume_set::VolumeSet;
use crate::{Hfid, Oid, Vfid, Vpid};

/// develop `heap_hdr_stats.next_vpid` / 11.4 the same field: the header record's link to the next page (@16).
const HEADER_NEXT_VPID: usize = 16;

/// Engine `spage_collect_statistics` of one page: 1 page + 2 per REC_BIGONE; records = live slots other than
/// slot 0 and REC_NEWHOME; length = REC_HOME and REC_NEWHOME lengths + 2 pages per REC_BIGONE.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PageStats {
    pub pages: u64,
    pub recs: u64,
    pub sumlen: u64,
}

impl PageStats {
    pub fn of(sp: &SlottedPage, db_pagesize: usize) -> PageStats {
        let mut s = PageStats {
            pages: 1,
            ..Default::default()
        };
        for slot in 1..sp.num_slots() {
            let Some((t, rec)) = sp.record(slot) else {
                continue;
            };
            if t == RecordType::BigOne {
                s.pages += 2;
                s.sumlen += 2 * db_pagesize as u64;
            }
            if t != RecordType::NewHome {
                s.recs += 1;
            }
            if matches!(t, RecordType::Home | RecordType::NewHome) {
                s.sumlen += rec.len() as u64;
            }
        }
        s
    }

    pub fn add(&mut self, o: PageStats) {
        self.pages += o.pages;
        self.recs += o.recs;
        self.sumlen += o.sumlen;
    }
}

/// One heap page the rewrite has read, as it stands after the last edit.
#[derive(Clone, Copy, Debug)]
pub struct PageInfo {
    pub vpid: Vpid,
    pub total_free: i32,
    /// Engine `spage_max_space_for_new_record`.
    pub max_space: usize,
    pub stats: PageStats,
}

fn max_space(sp: &SlottedPage) -> usize {
    let mut free = sp.total_free();
    let reuse = sp.anchor_raw() != Anchor::AnchoredDontReuseSlots as i16
        && sp.num_slots() > sp.num_records();
    if !reuse {
        free -= SPAGE_SLOT_SIZE as i32;
        if free < SPAGE_SLOT_SIZE as i32 {
            free = 0;
        }
    }
    let a = sp.alignment().max(1) as i32;
    (free.max(0) / a * a) as usize
}

pub(crate) fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

pub(crate) fn spage_err(vpid: Vpid, e: SpageError) -> io::Error {
    invalid(format!("heap page {vpid:?}: {e:?}"))
}

/// An editor over a whole IO page's user area.
pub(crate) fn editor(page: &mut [u8], vpid: Vpid) -> io::Result<SpageEditor<'_>> {
    SpageEditor::new(db_area_mut(page))
        .ok_or_else(|| invalid(format!("heap page {vpid:?} is not a slotted page")))
}

struct Current {
    vpid: Vpid,
    page: Box<[u8]>,
    dirty: bool,
}

pub(crate) struct HeapState<'s, 'v> {
    pub space: &'s SpaceManager<'v>,
    pub hfid: Hfid,
    pub class_oid: Oid,
    /// The heap file is `FILE_HEAP_REUSE_SLOTS`: a vacuumed home slot becomes REC_DELETED_WILL_REUSE.
    pub reuse_slots: bool,
    /// The overflow file; NULL until a big record needs one.
    pub ovf_vfid: Vfid,
    /// Last page of the chain (moves when a page is appended).
    pub last_vpid: Vpid,
    /// Space kept free on the header page for the larger develop header record until it is written.
    pub header_reserve: usize,
    cur: Option<Current>,
    pages: Vec<PageInfo>,
    index: HashMap<Vpid, usize>,
    pub appended_pages: u64,
}

impl<'s, 'v> HeapState<'s, 'v> {
    pub fn new(
        space: &'s SpaceManager<'v>,
        hfid: Hfid,
        class_oid: Oid,
        reuse_slots: bool,
        ovf_vfid: Vfid,
        last_vpid: Vpid,
    ) -> Self {
        HeapState {
            space,
            hfid,
            class_oid,
            reuse_slots,
            ovf_vfid,
            last_vpid,
            header_reserve: 0,
            cur: None,
            pages: Vec::new(),
            index: HashMap::new(),
            appended_pages: 0,
        }
    }

    pub fn vs(&self) -> &'v VolumeSet {
        self.space.volumes()
    }

    pub fn db_pagesize(&self) -> usize {
        db_pagesize(self.vs().io_pagesize())
    }

    /// Every heap page read so far, in the order first read.
    pub fn pages(&self) -> &[PageInfo] {
        &self.pages
    }

    fn note(&mut self, vpid: Vpid, page: &[u8]) -> io::Result<()> {
        let sp = SlottedPage::new(db_area(page))
            .ok_or_else(|| invalid(format!("heap page {vpid:?} is not a slotted page")))?;
        let info = PageInfo {
            vpid,
            total_free: sp.total_free(),
            max_space: max_space(&sp),
            stats: PageStats::of(&sp, self.db_pagesize()),
        };
        match self.index.get(&vpid) {
            Some(&i) => self.pages[i] = info,
            None => {
                self.index.insert(vpid, self.pages.len());
                self.pages.push(info);
            }
        }
        Ok(())
    }

    /// Writes the current page back if it changed.
    pub fn flush(&mut self) -> io::Result<()> {
        if let Some(c) = self.cur.take() {
            if c.dirty {
                self.vs().write_page(c.vpid, &c.page)?;
            }
            self.note(c.vpid, &c.page)?;
        }
        Ok(())
    }

    /// Makes `vpid` the current page (the previous one written back).
    pub fn load(&mut self, vpid: Vpid) -> io::Result<()> {
        self.flush()?;
        let page = self.vs().read_page(vpid)?;
        self.note(vpid, &page)?;
        self.cur = Some(Current {
            vpid,
            page,
            dirty: false,
        });
        Ok(())
    }

    pub fn current_vpid(&self) -> Vpid {
        self.cur.as_ref().map_or(Vpid::NULL, |c| c.vpid)
    }

    /// Reads heap page `vpid` (the current buffer when it is current).
    pub fn peek<R>(&self, vpid: Vpid, f: impl FnOnce(&[u8]) -> io::Result<R>) -> io::Result<R> {
        match &self.cur {
            Some(c) if c.vpid == vpid => f(&c.page),
            _ => f(&self.vs().read_page(vpid)?),
        }
    }

    /// Edits heap page `vpid` (whole IO page): in the current buffer when it is current, otherwise read, edited and
    /// written at once. The page counts as read.
    pub fn edit<R>(
        &mut self,
        vpid: Vpid,
        f: impl FnOnce(&mut [u8]) -> io::Result<R>,
    ) -> io::Result<R> {
        if let Some(c) = self.cur.as_mut().filter(|c| c.vpid == vpid) {
            c.dirty = true;
            return f(&mut c.page);
        }
        let mut page = self.vs().read_page(vpid)?;
        let r = f(&mut page)?;
        self.vs().write_page(vpid, &page)?;
        self.note(vpid, &page)?;
        Ok(r)
    }

    /// Rewrites the slot-0 record of heap page `vpid` through `f` (same length).
    pub fn edit_slot0(&mut self, vpid: Vpid, f: impl FnOnce(&mut [u8])) -> io::Result<()> {
        self.edit(vpid, |p| {
            let mut ed = editor(p, vpid)?;
            let mut rec = ed
                .view()
                .record(0)
                .ok_or_else(|| invalid(format!("heap page {vpid:?} has no slot 0")))?
                .1
                .to_vec();
            f(&mut rec);
            ed.update(0, &rec).map_err(|e| spage_err(vpid, e))
        })
    }

    /// Points page `vpid`'s chain at `next` (the header page's link lives in its header record).
    pub fn set_next(&mut self, vpid: Vpid, next: Vpid) -> io::Result<()> {
        let header = vpid == self.hfid.vpid();
        self.edit_slot0(vpid, |rec| {
            if header {
                next.write(rec, HEADER_NEXT_VPID);
            } else if let Some(mut c) = HeapChain::read(rec) {
                c.next_vpid = next;
                c.write(rec);
            }
        })
    }

    /// Points data page `vpid`'s chain back at `prev`.
    pub fn set_prev(&mut self, vpid: Vpid, prev: Vpid) -> io::Result<()> {
        self.edit_slot0(vpid, |rec| {
            if let Some(mut c) = HeapChain::read(rec) {
                c.prev_vpid = prev;
                c.write(rec);
            }
        })
    }

    /// Allocates a heap page, initializes it as `heap_vpid_init_new` does and links it after the chain end.
    pub fn append_page(&mut self) -> io::Result<Vpid> {
        let vpid = self.space.alloc_page(self.hfid.vfid, PageType::Heap)?;
        let mut p = vec![0u8; self.vs().io_pagesize()];
        page::init_header(&mut p, vpid, PageType::Heap);
        let mut chain = [0u8; HeapChain::SIZE];
        HeapChain {
            class_oid: self.class_oid,
            prev_vpid: self.last_vpid,
            next_vpid: Vpid::NULL,
            max_mvccid: 0,
            flags: 0,
        }
        .write(&mut chain);
        let mut b = SpageBuilder::new(db_area_mut(&mut p), Anchor::AnchoredDontReuseSlots, 4, true);
        b.insert(RecordType::Home, &chain);
        b.finish();
        self.vs().write_page(vpid, &p)?;
        self.note(vpid, &p)?;
        let last = self.last_vpid;
        self.set_next(last, vpid)?;
        self.last_vpid = vpid;
        self.appended_pages += 1;
        Ok(vpid)
    }

    /// Inserts `rec` as a REC_NEWHOME on the first page read so far (not `exclude`) with room, else on a new page at
    /// the chain end; returns its OID.
    // shortcut: first fit scans every page read (O(pages) per relocated record); upgrade to a free-space index if a
    // heap with many relocations makes the scan show up in measurements.
    pub fn insert_newhome(&mut self, rec: &[u8], exclude: Vpid) -> io::Result<Oid> {
        let header = self.hfid.vpid();
        let mut i = 0;
        while i < self.pages.len() {
            let info = self.pages[i];
            i += 1;
            let room = if info.vpid == header {
                info.max_space.saturating_sub(self.header_reserve)
            } else {
                info.max_space
            };
            if info.vpid == exclude || room < rec.len() {
                continue;
            }
            let vpid = info.vpid;
            let slot = self.edit(vpid, |p| {
                match editor(p, vpid)?.insert(RecordType::NewHome, rec) {
                    Ok(s) => Ok(Some(s)),
                    Err(SpageError::DoesntFit) => Ok(None),
                    Err(e) => Err(spage_err(vpid, e)),
                }
            })?;
            if let Some(s) = slot {
                return Ok(Oid::new(vpid.volid, vpid.pageid, s as i16));
            }
        }
        let vpid = self.append_page()?;
        let s = self.edit(vpid, |p| {
            editor(p, vpid)?
                .insert(RecordType::NewHome, rec)
                .map_err(|e| spage_err(vpid, e))
        })?;
        Ok(Oid::new(vpid.volid, vpid.pageid, s as i16))
    }
}
