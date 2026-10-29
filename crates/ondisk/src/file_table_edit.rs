//! Page allocation and deallocation in a permanent file whose partial and full sector tables may continue past
//! the header page, step for step as the engine's file manager does it.
//!
//! Input: a [`SpaceOp`] (the pages of one space operation, the disk cache, the target line) and a VFID.
//! Output: the file header, table pages and STAB bits as the engine leaves them after the same calls: a page taken
//! from the first partial sector of the header; the file expanded (1 % of its sectors, between its tablespace
//! minimum and the room left in the header's partial table) when no page is free; a filled sector moved to the
//! full table, which gets a new table page at the head of its chain when every component is full; a freed page of
//! a full sector moving the sector back to the partial table (a new partial table page at the end of its chain
//! when every component is full); a full table component merged into its neighbour after a removal; partial
//! sectors moved from the first chained page into an empty header table, that page reused when emptied.
//! Each component of a table is ordered by VSID; the chain as a whole is not.
//! Source: engine `src/storage/file_manager.c` `file_perm_alloc` (:5154-5326), `file_perm_expand` (:4632-4747),
//! `file_table_move_partial_sectors_to_header` (:4760-4913), `file_table_append_full_sector_page` (:4964-5003),
//! `file_table_add_full_sector` (:5014-5115), `file_perm_dealloc` (:6290-6586),
//! `file_extdata_find_and_remove_item` (:2547-2689), `file_extdata_find_not_full` (:2088-2137),
//! `file_header_alloc/dealloc` (:1069-1134); develop 682b080b2 (= 5b1867174 for these lines). Same rules in
//! release/11.4. No log records (ADR 0004); new and reused table pages get a NULL LSA (G2, workspace#482).

use std::io;

use crate::extdata::{self, Component, ExtdataHeader};
use crate::file_header::FileHeader;
use zerocopy::IntoBytes;

use crate::file_table::PartialSectorLe;
use crate::ids::{Vfid, Vpid, Vsid, VsidLe};

use crate::page::{self, PageType};
use crate::page_batch::PageBatch;
use crate::sector_reserve::DiskCache;
use crate::sector_table::SECTOR_NPAGES;
use crate::version_line::VersionLine;

/// `sizeof (FILE_PARTIAL_SECTOR)`.
pub const PARTSECT_SIZE: usize = size_of::<PartialSectorLe>();
/// `sizeof (VSID)`: a full table item.
pub const FULLSECT_SIZE: usize = size_of::<VsidLe>();
/// Longest table chain walked before the file is called corrupt.
const CHAIN_LIMIT: usize = 1 << 20;

/// Engine `FILE_ALLOC_TYPE`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AllocType {
    UserPage,
    /// A partial table page (or a tracker page counted as a table page).
    TablePage,
    /// A full table page, linked into the full table by the allocation itself.
    TablePageFullSector,
}

fn err(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

/// Engine `disk_compare_vsids` as a key.
pub fn vsid_key(item: &[u8]) -> Vsid {
    Vsid::read(item, 0)
}

/// One space operation: everything it edits, flushed at the end.
pub struct SpaceOp<'a, 'v> {
    pub pages: PageBatch<'v>,
    pub disk: &'a mut DiskCache,
    pub line: VersionLine,
}

impl SpaceOp<'_, '_> {
    pub fn header(&mut self, vfid: Vfid) -> io::Result<FileHeader> {
        Ok(FileHeader::read(self.pages.get(vfid.header_vpid())?))
    }

    pub fn put_header(&mut self, vfid: Vfid, h: &FileHeader) -> io::Result<()> {
        let line = self.line;
        h.write(self.pages.get(vfid.header_vpid())?, line);
        Ok(())
    }

    /// The components of a chain starting at (`first`, `at`), in order (pages are read into the batch).
    pub fn chain(&mut self, first: Vpid, at: usize) -> io::Result<Vec<Component>> {
        let mut out = Vec::new();
        let mut c = Component { vpid: first, at };
        loop {
            if out.len() >= CHAIN_LIMIT {
                return Err(err("table chain too long or cyclic".into()));
            }
            let next = ExtdataHeader::read(self.pages.get(c.vpid)?, c.at).vpid_next;
            out.push(c);
            if next.is_null() {
                return Ok(out);
            }
            c = Component { vpid: next, at: 0 };
        }
    }

    /// Engine `file_extdata_find_not_full`: the first component with room, or `Err(last component)`.
    pub fn find_not_full(
        &mut self,
        first: Vpid,
        at: usize,
    ) -> io::Result<Result<Component, Component>> {
        let comps = self.chain(first, at)?;
        for c in &comps {
            if extdata::remaining(self.pages.get(c.vpid)?, c.at) > 0 {
                return Ok(Ok(*c));
            }
        }
        Ok(Err(*comps.last().unwrap()))
    }

    /// Engine `file_extdata_find_and_remove_item` over a chain whose components are each ordered by `key`:
    /// removes the item equal to `want`, then merges the component into the previous one or the next one into it
    /// when they fit. Returns the removed item and the page of a component merged away (the caller deallocates it).
    pub fn find_and_remove<K: Ord + Copy>(
        &mut self,
        first: Vpid,
        at: usize,
        want: K,
        key: impl Fn(&[u8]) -> K + Copy,
    ) -> io::Result<(Vec<u8>, Option<Vpid>)> {
        let comps = self.chain(first, at)?;
        for (i, c) in comps.iter().enumerate() {
            let p = self.pages.get(c.vpid)?;
            let Ok(pos) = extdata::find_ordered(p, c.at, |it| key(it).cmp(&want)) else {
                continue;
            };
            let removed = extdata::item(p, c.at, pos).to_vec();
            extdata::remove_at(p, c.at, pos);
            if i > 0 && self.merge(comps[i], comps[i - 1], key)? {
                return Ok((removed, Some(c.vpid)));
            }
            if let Some(next) = comps.get(i + 1)
                && self.merge(*next, *c, key)?
            {
                return Ok((removed, Some(next.vpid)));
            }
            return Ok((removed, None));
        }
        Err(err("item to remove is not in the table".into()))
    }

    /// Engine `file_extdata_merge_pages` (ordered) of `src` into `dst`.
    fn merge<K: Ord>(
        &mut self,
        src: Component,
        dst: Component,
        key: impl Fn(&[u8]) -> K,
    ) -> io::Result<bool> {
        let (sp, dp) = self.pages.get2(src.vpid, dst.vpid)?;
        Ok(extdata::merge_ordered(sp, src.at, dp, dst.at, key))
    }

    /// Engine `file_perm_alloc`.
    pub fn perm_alloc(&mut self, vfid: Vfid, t: AllocType) -> io::Result<Vpid> {
        let hv = vfid.header_vpid();
        if self.header(vfid)?.n_page_free == 0 {
            self.perm_expand(vfid)?;
        }
        let pat = self.header(vfid)?.offset_to_partial_ftab as usize;
        if ExtdataHeader::read(self.pages.get(hv)?, pat).n_items == 0 {
            // free pages exist, so partial sectors are in later table pages
            if let Some(v) = self.move_partial_sectors_to_header(vfid, t)? {
                return Ok(v);
            }
        }
        let hp = self.pages.get(hv)?;
        let it = extdata::item_mut(hp, pat, 0);
        let ps = PartialSectorLe::of_mut(it);
        let vsid = ps.vsid.get();
        let bitmap = ps.page_bitmap.get();
        if bitmap == u64::MAX {
            return Err(err(format!(
                "file {vfid:?}: full sector in the partial table"
            )));
        }
        let bit = (!bitmap).trailing_zeros();
        ps.page_bitmap.set(bitmap | 1 << bit);
        let is_full = bitmap | 1 << bit == u64::MAX;
        let vpid = Vpid::new(vsid.volid, vsid.sectid * SECTOR_NPAGES + bit as i32);
        if t == AllocType::TablePageFullSector {
            self.append_full_sector_page(vfid, vpid)?;
        }
        let mut h = self.header(vfid)?;
        header_alloc(&mut h, t, bitmap == 0, is_full);
        self.put_header(vfid, &h)?;
        if is_full {
            // out of the partial table first: adding to the full table may allocate from the next partial sector
            extdata::remove_at(self.pages.get(hv)?, pat, 0);
            self.add_full_sector(vfid, vsid)?;
        }
        Ok(vpid)
    }

    /// Engine `file_perm_expand`: reserves sectors (sorted) and appends them, empty, to the header's partial table.
    fn perm_expand(&mut self, vfid: Vfid) -> io::Result<()> {
        let mut h = self.header(vfid)?;
        let db_sector =
            (page::db_pagesize(self.pages.volumes().io_pagesize()) * SECTOR_NPAGES as usize) as i32;
        let pat = h.offset_to_partial_ftab as usize;
        let room = extdata::remaining(self.pages.get(vfid.header_vpid())?, pat) as i32;
        let min = (h.tablespace.expand_min_size / db_sector).max(1);
        let max = (h.tablespace.expand_max_size / db_sector).min(room);
        if min > max {
            return Err(err(format!(
                "file {vfid:?}: expand min {min} > max {max} sectors (partial table room {room})"
            )));
        }
        let n = ((h.n_sector_total as f32 * h.tablespace.expand_ratio) as i32).clamp(min, max);
        let vols = self.pages.volumes();
        let mut vsids = self.disk.reserve(vols, n)?;
        vsids.sort();
        let hp = self.pages.get(vfid.header_vpid())?;
        for v in &vsids {
            extdata::append_items(hp, pat, PartialSectorLe::new(*v, 0).as_bytes());
        }
        h.n_sector_total += n;
        h.n_sector_empty = n;
        h.n_sector_partial = n;
        h.n_page_free = n * SECTOR_NPAGES;
        h.n_page_total += h.n_page_free;
        self.put_header(vfid, &h)
    }

    /// Engine `file_table_move_partial_sectors_to_header`: moves the first chained partial page's items into the
    /// empty header table; an emptied page leaves the chain and is returned for reuse by this allocation.
    fn move_partial_sectors_to_header(
        &mut self,
        vfid: Vfid,
        t: AllocType,
    ) -> io::Result<Option<Vpid>> {
        let hv = vfid.header_vpid();
        let pat = self.header(vfid)?.offset_to_partial_ftab as usize;
        let first = ExtdataHeader::read(self.pages.get(hv)?, pat).vpid_next;
        if first.is_null() {
            return Err(err(format!(
                "file {vfid:?}: free pages but no partial sector"
            )));
        }
        let (fp, hp) = self.pages.get2(first, hv)?;
        let fh = ExtdataHeader::read(fp, 0);
        let n = fh.n_items as usize;
        if n == 0 {
            return Err(err(format!(
                "file {vfid:?}: empty partial table page {first:?}"
            )));
        }
        let k = n.min(extdata::remaining(hp, pat));
        extdata::append_items(hp, pat, extdata::items(fp, 0, 0, k));
        if k < n {
            extdata::remove_range(fp, 0, 0, k);
            return Ok(None);
        }
        let mut head = ExtdataHeader::read(hp, pat);
        head.vpid_next = fh.vpid_next;
        head.write(hp, pat);
        // pgbuf_dealloc_page: the page is "deallocated" and handed to this allocation
        page::set_ptype(fp, PageType::Unknown);
        match t {
            AllocType::TablePageFullSector => self.append_full_sector_page(vfid, first)?,
            AllocType::UserPage => {
                let mut h = self.header(vfid)?;
                h.n_page_ftab -= 1;
                h.n_page_user += 1;
                self.put_header(vfid, &h)?;
            }
            AllocType::TablePage => {}
        }
        Ok(Some(first))
    }

    /// Engine `file_table_append_full_sector_page`: `vpid` becomes an empty full table page linked right after
    /// the header's component.
    fn append_full_sector_page(&mut self, vfid: Vfid, vpid: Vpid) -> io::Result<()> {
        let hv = vfid.header_vpid();
        let fat = self.header(vfid)?.offset_to_full_ftab as usize;
        let mut head = ExtdataHeader::read(self.pages.get(hv)?, fat);
        let np = self.pages.blank(vpid);
        page::init_header(np, vpid, PageType::Ftab);
        extdata::init_page(np, FULLSECT_SIZE);
        let mut eh = ExtdataHeader::read(np, 0);
        eh.vpid_next = head.vpid_next;
        eh.write(np, 0);
        head.vpid_next = vpid;
        head.write(self.pages.get(hv)?, fat);
        Ok(())
    }

    /// Engine `file_table_add_full_sector`: into the first full table component with room, at its ordered place;
    /// a new table page when none has room.
    fn add_full_sector(&mut self, vfid: Vfid, vsid: Vsid) -> io::Result<()> {
        let fat = self.header(vfid)?.offset_to_full_ftab as usize;
        let c = match self.find_not_full(vfid.header_vpid(), fat)? {
            Ok(c) => c,
            Err(_) => Component {
                vpid: self.perm_alloc(vfid, AllocType::TablePageFullSector)?,
                at: 0,
            },
        };
        let p = self.pages.get(c.vpid)?;
        let pos = extdata::find_ordered(p, c.at, |it| vsid_key(it).cmp(&vsid))
            .err()
            .ok_or_else(|| err(format!("sector {vsid:?} already in the full table")))?;
        let mut b = [0u8; FULLSECT_SIZE];
        vsid.write(&mut b, 0);
        extdata::insert_at(p, c.at, pos, &b);
        Ok(())
    }

    /// Engine `file_perm_dealloc`: the page's bit cleared in its partial sector, or its full sector moved back to
    /// the partial table; the page set to `PAGE_UNKNOWN` (`pgbuf_dealloc_page`). Sectors stay with the file.
    pub fn perm_dealloc(&mut self, vfid: Vfid, vpid: Vpid, t: AllocType) -> io::Result<()> {
        let h = self.header(vfid)?;
        let (pat, fat) = (
            h.offset_to_partial_ftab as usize,
            h.offset_to_full_ftab as usize,
        );
        let hv = vfid.header_vpid();
        let vsid = Vsid::new(vpid.volid, vpid.pageid / SECTOR_NPAGES);
        let bit = 1u64 << (vpid.pageid % SECTOR_NPAGES);
        let mut is_empty = false;
        let mut was_full = false;
        let mut found = None;
        for c in self.chain(hv, pat)? {
            let p = self.pages.get(c.vpid)?;
            if let Ok(i) = extdata::find_ordered(p, c.at, |it| vsid_key(it).cmp(&vsid)) {
                found = Some((c, i));
                break;
            }
        }
        if let Some((c, i)) = found {
            let it = extdata::item_mut(self.pages.get(c.vpid)?, c.at, i);
            let ps = PartialSectorLe::of_mut(it);
            let bm = ps.page_bitmap.get();
            if bm & bit == 0 {
                return Err(err(format!("page {vpid:?} is not allocated in {vfid:?}")));
            }
            ps.page_bitmap.set(bm & !bit);
            is_empty = bm & !bit == 0;
        } else {
            was_full = true;
            let (_, merged) = self.find_and_remove(hv, fat, vsid, vsid_key)?;
            let mut bitmap = !bit; // FILE_FULL_PAGE_BITMAP less the page
            if let Some(m) = merged {
                if m.volid == vpid.volid && m.pageid / SECTOR_NPAGES == vsid.sectid {
                    // the merged table page is in the sector going back to the partial table
                    bitmap &= !(1u64 << (m.pageid % SECTOR_NPAGES));
                    let mut h = self.header(vfid)?;
                    header_dealloc(&mut h, AllocType::TablePage, false, false);
                    self.put_header(vfid, &h)?;
                    page::set_ptype(self.pages.get(m)?, PageType::Unknown);
                } else {
                    self.perm_dealloc(vfid, m, AllocType::TablePage)?;
                }
            }
            let ps = PartialSectorLe::new(vsid, bitmap);
            match self.find_not_full(hv, pat)? {
                Ok(c) => {
                    let p = self.pages.get(c.vpid)?;
                    let pos = extdata::find_ordered(p, c.at, |it| vsid_key(it).cmp(&vsid))
                        .err()
                        .ok_or_else(|| {
                            err(format!("sector {vsid:?} already in the partial table"))
                        })?;
                    extdata::insert_at(p, c.at, pos, ps.as_bytes());
                }
                Err(last) => {
                    let nv = self.perm_alloc(vfid, AllocType::TablePage)?;
                    let lp = self.pages.get(last.vpid)?;
                    let mut lh = ExtdataHeader::read(lp, last.at);
                    lh.vpid_next = nv;
                    lh.write(lp, last.at);
                    let np = self.pages.blank(nv);
                    page::init_header(np, nv, PageType::Ftab);
                    extdata::init_page(np, PARTSECT_SIZE);
                    extdata::append_items(np, 0, ps.as_bytes());
                }
            }
        }
        let mut h = self.header(vfid)?;
        header_dealloc(&mut h, t, is_empty, was_full);
        self.put_header(vfid, &h)?;
        page::set_ptype(self.pages.get(vpid)?, PageType::Unknown);
        Ok(())
    }
}

/// Engine `file_header_alloc`.
fn header_alloc(h: &mut FileHeader, t: AllocType, was_empty: bool, is_full: bool) {
    h.n_page_free -= 1;
    if t == AllocType::UserPage {
        h.n_page_user += 1;
    } else {
        h.n_page_ftab += 1;
    }
    if was_empty {
        h.n_sector_empty -= 1;
    }
    if is_full {
        h.n_sector_partial -= 1;
        h.n_sector_full += 1;
    }
}

/// Engine `file_header_dealloc`.
fn header_dealloc(h: &mut FileHeader, t: AllocType, is_empty: bool, was_full: bool) {
    h.n_page_free += 1;
    if t == AllocType::UserPage {
        h.n_page_user -= 1;
    } else {
        h.n_page_ftab -= 1;
    }
    if is_empty {
        h.n_sector_empty += 1;
    }
    if was_full {
        h.n_sector_partial += 1;
        h.n_sector_full -= 1;
    }
}
