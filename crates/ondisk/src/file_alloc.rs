//! Space management on a target database: reserve and release sectors, create and destroy permanent files,
//! allocate and deallocate their pages, register files in the file tracker.
//!
//! Input: a writable [`VolumeSet`], the tracker VFID and the version line whose file header layout to write.
//! Output: the pages, file headers, file tables, tracker and sector tables rewritten as the engine's file manager
//! would leave them after the same operations (no log records, ADR 0004).
//! Source: engine `src/storage/file_manager.c` `file_create` (:3440-3830), `file_perm_expand` (:4632-4747),
//! `file_perm_alloc` (:5154-5326), `file_perm_dealloc` (:6290-6586), `file_header_alloc/dealloc` (:1069-1134),
//! `file_tracker_register/unregister`, `file_destroy`; `disk_manager.c` `disk_reserve_sectors`,
//! `disk_unreserve_ordered_sectors`; `page_buffer.c` `pgbuf_dealloc_page`. R5 § 3 F1-F4, N1-N5. New pages get a
//! NULL LSA (G2, workspace#482).
//!
//! One mutex serializes every space change (they all touch headers and the sector table); writing the contents of
//! an allocated page needs no lock ([`VolumeSet::write_page`]).

use std::io;
use std::sync::Mutex;

use crate::extdata::{self, ExtdataHeader};
use crate::file_header::{FileHeader, FileType, Tablespace};
use crate::file_table::FileTable;
use crate::file_tracker::{self, TRACK_ITEM_SIZE, TrackItem};
use crate::ids::{Vfid, Vpid, Vsid};
use crate::le_field::{get_u64, put_u64};
use crate::page::{self, PageType, db_pagesize};
use crate::sector_table::{self, SECTOR_NPAGES};
use crate::version_line::VersionLine;
use crate::volume_set::VolumeSet;

/// `sizeof (FILE_PARTIAL_SECTOR)`.
const PARTSECT_SIZE: usize = 16;

fn err(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

/// Engine `FILE_TABLESPACE_FOR_PERM_NPAGES`: initial size `npages` pages, grow 1 %, at least one sector, at most
/// 1024 sectors at a time.
pub fn perm_tablespace(io_pagesize: usize, npages: i32) -> Tablespace {
    let db = db_pagesize(io_pagesize) as i64;
    Tablespace {
        initial_size: npages.max(1) as i64 * db,
        expand_ratio: 0.01,
        expand_min_size: (SECTOR_NPAGES as i64 * db) as i32,
        expand_max_size: (SECTOR_NPAGES as i64 * db * 1024) as i32,
    }
}

/// Writer of space changes on one database.
pub struct SpaceManager<'v> {
    vols: &'v VolumeSet,
    line: VersionLine,
    trk_vfid: Vfid,
    lock: Mutex<()>,
}

impl<'v> SpaceManager<'v> {
    /// `line` is the layout new file headers are written in (the target's line).
    pub fn new(vols: &'v VolumeSet, line: VersionLine, trk_vfid: Vfid) -> Self {
        SpaceManager {
            vols,
            line,
            trk_vfid,
            lock: Mutex::new(()),
        }
    }

    pub fn volumes(&self) -> &'v VolumeSet {
        self.vols
    }

    // ---- sectors ----

    /// Engine `disk_reserve_sectors` for one sector: the lowest free sector, trying `prefer` first, then the other
    /// volumes in volid order. Only sectors below `nsect_total` are used; growing a volume is not done here.
    // shortcut: no volume extension (nsect_total..nsect_max) or new volume; upgrade when a target runs out of space
    // (map workspace#472 Not yet specified).
    fn reserve_sector(&self, prefer: i16) -> io::Result<Vsid> {
        let n = self.vols.volumes().len() as i16;
        let order = std::iter::once(prefer).chain((0..n).filter(|&v| v != prefer));
        for volid in order.filter(|&v| v >= 0 && v < n) {
            let vh = &self.vols.volume(volid)?.header;
            let used = sector_table::used_sectors(self.vols, volid)?;
            let first = sector_table::system_sectors(vh).end() + 1;
            let Some(sectid) = (first..vh.nsect_total).find(|s| used.binary_search(s).is_err())
            else {
                continue;
            };
            self.set_sector_bit(Vsid::new(volid, sectid), true)?;
            return Ok(Vsid::new(volid, sectid));
        }
        Err(err("no free sector in any volume".into()))
    }

    /// Sets or clears a sector's STAB bit; refuses a bit that already has the wanted value (two owners, or a
    /// sector released twice).
    fn set_sector_bit(&self, vsid: Vsid, on: bool) -> io::Result<()> {
        let vh = &self.vols.volume(vsid.volid)?.header;
        let (vpid, unit, bit) =
            sector_table::bit_position(vh, self.vols.io_pagesize(), vsid.sectid);
        let mut p = self.vols.read_page(vpid)?;
        if sector_table::set_bit(&mut p, unit, bit, on) == on {
            return Err(err(format!(
                "sector {}|{} already {}",
                vsid.volid,
                vsid.sectid,
                if on { "set" } else { "clear" }
            )));
        }
        self.vols.write_page(vpid, &p)
    }

    // ---- files ----

    /// Engine `file_create` for a permanent, non-numerable file of `ftype` with one sector: header page = first
    /// page of the sector, partial table then full table in the header page (`line`'s header size), registered
    /// in the tracker. `descriptor` is the raw 64-byte descriptor union.
    // shortcut: numerable files (extendible hash) are not created; upgrade if catalog regeneration needs a new one.
    pub fn create_file(
        &self,
        ftype: FileType,
        descriptor: [u8; 64],
        tablespace: Tablespace,
    ) -> io::Result<Vfid> {
        let _g = self.lock.lock().unwrap();
        let vsid = self.reserve_sector(0)?;
        let vfid = Vfid::new(vsid.volid, vsid.sectid * SECTOR_NPAGES);
        let io = self.vols.io_pagesize();
        let db = db_pagesize(io);
        let hsize = self.line.file_header_size();
        let part = ExtdataHeader::init((db - hsize) / 2, PARTSECT_SIZE);
        let full_at = hsize + part.footprint();
        let full = ExtdataHeader::init(db - full_at, 8);

        let mut hp = vec![0u8; io];
        page::init_header(&mut hp, vfid.header_vpid(), PageType::Ftab);
        let header = FileHeader {
            time_creation: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            self_vfid: vfid,
            tablespace,
            descriptor_raw: descriptor,
            n_page_total: SECTOR_NPAGES,
            n_page_user: 0,
            n_page_ftab: 1,
            n_page_free: SECTOR_NPAGES - 1,
            n_page_mark_delete: 0,
            n_sector_total: 1,
            n_sector_partial: 1,
            n_sector_full: 0,
            n_sector_empty: 0,
            file_type_raw: ftype as i32,
            file_flags: 0,
            volid_last_expand: vsid.volid,
            offset_to_partial_ftab: hsize as i16,
            offset_to_full_ftab: full_at as i16,
            offset_to_user_page_ftab: -1,
            vpid_sticky_first: Vpid::NULL,
            vpid_last_temp_alloc: Vpid::NULL,
            offset_to_last_temp_alloc: -1,
            vpid_last_user_page_ftab: Vpid::NULL,
        };
        header.write(&mut hp, self.line);
        part.write(&mut hp, hsize);
        full.write(&mut hp, full_at);
        let mut ps = [0u8; PARTSECT_SIZE];
        vsid.write(&mut ps, 0);
        put_u64(&mut ps, 8, 1); // the header page itself
        extdata::insert_at(&mut hp, hsize, 0, &ps);
        self.vols.write_page(vfid.header_vpid(), &hp)?;

        self.tracker_insert(TrackItem {
            vfid,
            file_type_raw: ftype as i16,
            metadata: 0,
        })?;
        Ok(vfid)
    }

    /// Engine `file_destroy` of a permanent file: every sector released in the STAB, every allocated page set to
    /// `PAGE_UNKNOWN` (as `pgbuf_dealloc_page` leaves it), the tracker entry removed (R5 F1-F4).
    pub fn destroy_file(&self, vfid: Vfid) -> io::Result<()> {
        let _g = self.lock.lock().unwrap();
        let ft = FileTable::read(self.vols, vfid)?;
        let issues = ft.check();
        if !issues.is_empty() {
            return Err(err(format!(
                "file {}|{} inconsistent: {issues:?}",
                vfid.volid, vfid.fileid
            )));
        }
        for v in ft.allocated_pages() {
            let mut p = self.vols.read_page(v)?;
            page::set_ptype(&mut p, PageType::Unknown);
            p[page::off::PFLAG] = 0;
            self.vols.write_page(v, &p)?;
        }
        for s in &ft.sectors {
            self.set_sector_bit(s.vsid, false)?;
        }
        self.tracker_remove(vfid)
    }

    // ---- pages ----

    /// Engine `file_perm_alloc` of a user page: a page of the first non-full partial sector (expanding the file by
    /// one sector when none is left), moved to the full table when its sector fills. The page is written with a
    /// header naming it, `ptype`, NULL LSA, and a zeroed body.
    pub fn alloc_page(&self, vfid: Vfid, ptype: PageType) -> io::Result<Vpid> {
        let _g = self.lock.lock().unwrap();
        self.alloc_page_locked(vfid, ptype, false)
    }

    /// Like [`Self::alloc_page`] and records the page as the file's sticky first page (never deallocated; a heap's
    /// header page, a B-tree root, the tracker's item page). Refuses a file that already has one.
    pub fn alloc_sticky_first_page(&self, vfid: Vfid, ptype: PageType) -> io::Result<Vpid> {
        let _g = self.lock.lock().unwrap();
        self.alloc_page_locked(vfid, ptype, true)
    }

    fn alloc_page_locked(&self, vfid: Vfid, ptype: PageType, sticky: bool) -> io::Result<Vpid> {
        let hv = vfid.header_vpid();
        let mut hp = self.vols.read_page(hv)?;
        let mut h = FileHeader::read(&hp);
        if h.is_numerable() || h.is_temporary() {
            return Err(err(
                "alloc_page: numerable and temporary files are not supported".into(),
            ));
        }
        if sticky && !h.vpid_sticky_first.is_null() {
            return Err(err("file already has a sticky first page".into()));
        }
        let pat = h.offset_to_partial_ftab as usize;
        if !ExtdataHeader::read(&hp, pat).vpid_next.is_null() {
            // shortcut: tables spilling past the header page are read but not modified; upgrade for files with
            // more than ~500 partial or ~1000 full sectors (16 KiB pages).
            return Err(err(
                "alloc_page: partial table continues past the header page".into(),
            ));
        }
        if h.n_page_free == 0 {
            if ExtdataHeader::read(&hp, pat).n_items as usize
                >= ExtdataHeader::read(&hp, pat).capacity()
            {
                return Err(err(
                    "alloc_page: partial table in the header page is full".into()
                ));
            }
            let vsid = self.reserve_sector(h.volid_last_expand)?;
            let mut ps = [0u8; PARTSECT_SIZE];
            vsid.write(&mut ps, 0);
            let pos = partial_position(&hp, pat, vsid);
            extdata::insert_at(&mut hp, pat, pos, &ps);
            h.n_sector_total += 1;
            h.n_sector_partial += 1;
            h.n_sector_empty += 1;
            h.n_page_total += SECTOR_NPAGES;
            h.n_page_free += SECTOR_NPAGES;
            h.volid_last_expand = vsid.volid;
        }
        // first partial sector with a free page (the engine keeps full ones out of the partial table)
        let n = ExtdataHeader::read(&hp, pat).n_items as usize;
        let idx = (0..n)
            .find(|&i| get_u64(extdata::item(&hp, pat, i), 8) != u64::MAX)
            .ok_or_else(|| {
                err("free page count > 0 but no partial sector has a free page".into())
            })?;
        let it = extdata::item_mut(&mut hp, pat, idx);
        let vsid = Vsid::read(it, 0);
        let bitmap = get_u64(it, 8);
        let bit = (!bitmap).trailing_zeros();
        let bitmap = bitmap | 1 << bit;
        put_u64(it, 8, bitmap);
        let vpid = Vpid::new(vsid.volid, vsid.sectid * SECTOR_NPAGES + bit as i32);

        h.n_page_free -= 1;
        h.n_page_user += 1;
        if bitmap.count_ones() == 1 {
            h.n_sector_empty -= 1;
        }
        if bitmap == u64::MAX {
            let fat = h.offset_to_full_ftab as usize;
            let fh = ExtdataHeader::read(&hp, fat);
            if !fh.vpid_next.is_null() || fh.n_items as usize >= fh.capacity() {
                return Err(err(
                    "alloc_page: full table in the header page is full or chained".into(),
                ));
            }
            extdata::remove_at(&mut hp, pat, idx);
            let mut b = [0u8; 8];
            vsid.write(&mut b, 0);
            let pos = (0..fh.n_items as usize)
                .find(|&i| Vsid::read(extdata::item(&hp, fat, i), 0) > vsid)
                .unwrap_or(fh.n_items as usize);
            extdata::insert_at(&mut hp, fat, pos, &b);
            h.n_sector_partial -= 1;
            h.n_sector_full += 1;
        }
        if sticky {
            h.vpid_sticky_first = vpid;
        }
        h.write(&mut hp, self.line);

        let mut np = vec![0u8; self.vols.io_pagesize()];
        page::init_header(&mut np, vpid, ptype);
        self.vols.write_page(vpid, &np)?;
        self.vols.write_page(hv, &hp)?;
        Ok(vpid)
    }

    /// Engine `file_perm_dealloc` of a user page: its bit cleared in the partial table, or its sector moved from
    /// the full table back to the partial table; the page set to `PAGE_UNKNOWN`. Refuses the header page and the
    /// sticky first page. Sectors stay reserved by the file, as in the engine.
    pub fn dealloc_page(&self, vfid: Vfid, vpid: Vpid) -> io::Result<()> {
        let _g = self.lock.lock().unwrap();
        let hv = vfid.header_vpid();
        let mut hp = self.vols.read_page(hv)?;
        let mut h = FileHeader::read(&hp);
        if vpid == hv || vpid == h.vpid_sticky_first || h.is_numerable() {
            return Err(err(
                "dealloc_page: header, sticky or numerable-file page".into()
            ));
        }
        let (pat, fat) = (
            h.offset_to_partial_ftab as usize,
            h.offset_to_full_ftab as usize,
        );
        if !ExtdataHeader::read(&hp, pat).vpid_next.is_null()
            || !ExtdataHeader::read(&hp, fat).vpid_next.is_null()
        {
            return Err(err(
                "dealloc_page: tables continue past the header page".into()
            ));
        }
        let vsid = Vsid::new(vpid.volid, vpid.pageid / SECTOR_NPAGES);
        let bit = 1u64 << (vpid.pageid % SECTOR_NPAGES);
        let np = ExtdataHeader::read(&hp, pat).n_items as usize;
        if let Some(i) = (0..np).find(|&i| Vsid::read(extdata::item(&hp, pat, i), 0) == vsid) {
            let it = extdata::item_mut(&mut hp, pat, i);
            let bm = get_u64(it, 8);
            if bm & bit == 0 {
                return Err(err(format!(
                    "page {}|{} is not allocated",
                    vpid.volid, vpid.pageid
                )));
            }
            put_u64(it, 8, bm & !bit);
            if bm & !bit == 0 {
                h.n_sector_empty += 1;
            }
        } else {
            let nf = ExtdataHeader::read(&hp, fat).n_items as usize;
            let i = (0..nf)
                .find(|&i| Vsid::read(extdata::item(&hp, fat, i), 0) == vsid)
                .ok_or_else(|| err(format!("page {}|{} not in file", vpid.volid, vpid.pageid)))?;
            extdata::remove_at(&mut hp, fat, i);
            let mut ps = [0u8; PARTSECT_SIZE];
            vsid.write(&mut ps, 0);
            put_u64(&mut ps, 8, !bit);
            let pos = partial_position(&hp, pat, vsid);
            if !extdata::insert_at(&mut hp, pat, pos, &ps) {
                return Err(err(
                    "dealloc_page: partial table in the header page is full".into(),
                ));
            }
            h.n_sector_full -= 1;
            h.n_sector_partial += 1;
        }
        h.n_page_free += 1;
        h.n_page_user -= 1;
        h.write(&mut hp, self.line);
        let mut p = self.vols.read_page(vpid)?;
        page::set_ptype(&mut p, PageType::Unknown);
        self.vols.write_page(vpid, &p)?;
        self.vols.write_page(hv, &hp)
    }

    // ---- tracker ----

    /// Engine `file_tracker_register`: inserts the item in (volid, fileid) order.
    // shortcut: only a single-page tracker is edited (holds ~1000 files at 16 KiB); upgrade for bigger catalogs.
    fn tracker_insert(&self, item: TrackItem) -> io::Result<()> {
        let fp = file_tracker::first_page(self.vols, self.trk_vfid)?;
        let mut p = self.vols.read_page(fp)?;
        let h = ExtdataHeader::read(&p, 0);
        if !h.vpid_next.is_null() {
            return Err(err("tracker spans more than one page".into()));
        }
        let key = (item.vfid.volid, item.vfid.fileid);
        let pos = (0..h.n_items as usize)
            .find(|&i| {
                let t = TrackItem::decode(extdata::item(&p, 0, i));
                (t.vfid.volid, t.vfid.fileid) >= key
            })
            .unwrap_or(h.n_items as usize);
        if pos < h.n_items as usize
            && TrackItem::decode(extdata::item(&p, 0, pos)).vfid == item.vfid
        {
            return Err(err("file already tracked".into()));
        }
        let bytes: [u8; TRACK_ITEM_SIZE] = item.encode();
        if !extdata::insert_at(&mut p, 0, pos, &bytes) {
            return Err(err("tracker page is full".into()));
        }
        self.vols.write_page(fp, &p)
    }

    /// Engine `file_tracker_unregister`.
    fn tracker_remove(&self, vfid: Vfid) -> io::Result<()> {
        let fp = file_tracker::first_page(self.vols, self.trk_vfid)?;
        let mut p = self.vols.read_page(fp)?;
        let h = ExtdataHeader::read(&p, 0);
        if !h.vpid_next.is_null() {
            return Err(err("tracker spans more than one page".into()));
        }
        let i = (0..h.n_items as usize)
            .find(|&i| TrackItem::decode(extdata::item(&p, 0, i)).vfid == vfid)
            .ok_or_else(|| err(format!("file {}|{} not tracked", vfid.volid, vfid.fileid)))?;
        extdata::remove_at(&mut p, 0, i);
        self.vols.write_page(fp, &p)
    }
}

/// Where `vsid` goes in the VSID-ordered partial table at `pat`.
fn partial_position(hp: &[u8], pat: usize, vsid: Vsid) -> usize {
    let n = ExtdataHeader::read(hp, pat).n_items as usize;
    (0..n)
        .find(|&i| Vsid::read(extdata::item(hp, pat, i), 0) > vsid)
        .unwrap_or(n)
}

/// One file of a [`SpaceManager`]: allocate pages and write them from any thread (the shape a B-tree builder's
/// page sink needs).
pub struct FilePages<'s, 'v> {
    pub space: &'s SpaceManager<'v>,
    pub vfid: Vfid,
}

impl FilePages<'_, '_> {
    pub fn alloc_page(&self, ptype: PageType) -> io::Result<Vpid> {
        self.space.alloc_page(self.vfid, ptype)
    }

    /// Writes a whole page (length = io page size). The caller keeps the page header (VPID, type, LSA) it got.
    pub fn write_page(&self, vpid: Vpid, buf: &[u8]) -> io::Result<()> {
        self.space.vols.write_page(vpid, buf)
    }

    pub fn io_pagesize(&self) -> usize {
        self.space.vols.io_pagesize()
    }
}
