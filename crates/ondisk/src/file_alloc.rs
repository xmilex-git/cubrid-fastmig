//! Space management on a target database: create and destroy permanent files, allocate and deallocate their
//! pages, register files in the file tracker, reserve and release sectors.
//!
//! Input: a writable [`VolumeSet`], the tracker VFID, the version line whose file header layout to write and how
//! the target may grow ([`VolumeGrowth`], engine default unless set).
//! Output: the pages, file headers, file tables, tracker, sector tables and volumes rewritten as the engine's file
//! and disk managers would leave them after the same operations (no log records, ADR 0004). File tables and the
//! tracker may span many pages ([`crate::file_table_edit`], [`crate::file_tracker_edit`]); a target out of space
//! grows its last volume and adds volumes like the engine ([`crate::sector_reserve`], ADR 0013, workspace#500).
//! Source: engine `src/storage/file_manager.c` `file_create` (:3440-3830), `file_alloc`, `file_dealloc`,
//! `file_destroy`, `file_tracker_register/unregister`; `disk_manager.c` `disk_reserve_sectors`,
//! `disk_unreserve_ordered_sectors`; `page_buffer.c` `pgbuf_dealloc_page`. R5 § 3 F1-F4, N1-N5. New pages get a
//! NULL LSA (G2, workspace#482).
//!
//! One mutex serializes every space change (they all touch headers, tables and the sector table); writing the
//! contents of an allocated page needs no lock ([`VolumeSet::write_page`]).

use std::io;
use std::sync::Mutex;

use crate::extdata::{self, ExtdataHeader};
use crate::file_header::{FileHeader, FileType, Tablespace};
use crate::file_table::FileTable;
use crate::file_table_edit::{AllocType, PARTSECT_SIZE, SpaceOp};
use crate::file_tracker::TrackItem;
use crate::ids::{Vfid, Vpid};
use crate::le_field::put_u64;
use crate::page::{self, PageType, db_pagesize};
use crate::page_batch::PageBatch;
use crate::sector_reserve::{DiskCache, VolumeGrowth};
use crate::sector_table::SECTOR_NPAGES;
use crate::version_line::VersionLine;
use crate::volume_set::VolumeSet;

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
    growth: VolumeGrowth,
    /// The disk cache, loaded from the STABs at the first space change.
    disk: Mutex<Option<DiskCache>>,
}

impl<'v> SpaceManager<'v> {
    /// `line` is the layout new file headers are written in (the target's line). The target grows with the
    /// engine's default `db_volume_size`, added volumes next to volume 0 and no `_vinf` line; see
    /// [`Self::with_growth`].
    pub fn new(vols: &'v VolumeSet, line: VersionLine, trk_vfid: Vfid) -> Self {
        SpaceManager {
            vols,
            line,
            trk_vfid,
            growth: VolumeGrowth::engine_default(vols.io_pagesize()),
            disk: Mutex::new(None),
        }
    }

    /// How the target grows when its volumes are full (set before the first space change).
    pub fn with_growth(mut self, growth: VolumeGrowth) -> Self {
        self.growth = growth;
        self
    }

    /// The file tracker the space changes register files in.
    pub fn trk_vfid(&self) -> Vfid {
        self.trk_vfid
    }

    pub fn volumes(&self) -> &'v VolumeSet {
        self.vols
    }

    /// Runs one space operation under the lock; its page edits are written only when it succeeds.
    fn run<T>(&self, f: impl FnOnce(&mut SpaceOp) -> io::Result<T>) -> io::Result<T> {
        let mut g = self.disk.lock().unwrap();
        if g.is_none() {
            *g = Some(DiskCache::load(self.vols, self.growth.clone())?);
        }
        let mut op = SpaceOp {
            pages: PageBatch::new(self.vols),
            disk: g.as_mut().unwrap(),
            line: self.line,
        };
        let out = f(&mut op)?;
        op.pages.flush()?;
        Ok(out)
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
        let trk = self.trk_vfid;
        self.run(|op| {
            let vols = op.pages.volumes();
            let vsid = op.disk.reserve(vols, 1)?[0];
            let vfid = Vfid::new(vsid.volid, vsid.sectid * SECTOR_NPAGES);
            let db = db_pagesize(vols.io_pagesize());
            let hsize = op.line.file_header_size();
            let part = ExtdataHeader::init((db - hsize) / 2, PARTSECT_SIZE);
            let full_at = hsize + part.footprint();
            let full = ExtdataHeader::init(db - full_at, 8);

            let hp = op.pages.blank(vfid.header_vpid());
            page::init_header(hp, vfid.header_vpid(), PageType::Ftab);
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
            header.write(hp, op.line);
            part.write(hp, hsize);
            full.write(hp, full_at);
            let mut ps = [0u8; PARTSECT_SIZE];
            vsid.write(&mut ps, 0);
            put_u64(&mut ps, 8, 1); // the header page itself
            extdata::insert_at(hp, hsize, 0, &ps);

            op.tracker_register(
                trk,
                TrackItem {
                    vfid,
                    file_type_raw: ftype as i16,
                    metadata: 0,
                },
            )?;
            Ok(vfid)
        })
    }

    /// Engine `file_destroy` of a permanent file: every allocated page set to `PAGE_UNKNOWN` (as
    /// `pgbuf_dealloc_page` leaves it), every sector released in the STAB, the tracker entry removed (R5 F1-F4).
    pub fn destroy_file(&self, vfid: Vfid) -> io::Result<()> {
        let trk = self.trk_vfid;
        self.run(|op| {
            let vols = op.pages.volumes();
            let ft = FileTable::read(vols, vfid)?;
            let issues = ft.check();
            if !issues.is_empty() {
                return Err(err(format!(
                    "file {}|{} inconsistent: {issues:?}",
                    vfid.volid, vfid.fileid
                )));
            }
            for v in ft.allocated_pages() {
                let p = op.pages.get(v)?;
                page::set_ptype(p, PageType::Unknown);
                p[page::off::PFLAG] = 0;
            }
            for s in &ft.sectors {
                op.disk.release(vols, s.vsid)?;
            }
            op.tracker_unregister(trk, vfid)?;
            Ok(())
        })
    }

    /// Engine `file_descriptor_update`: rewrites the 64-byte descriptor of `vfid` (a heap names its own HFID, which
    /// is known only after its sticky first page is allocated).
    pub fn set_descriptor(&self, vfid: Vfid, descriptor: [u8; 64]) -> io::Result<()> {
        self.run(|op| {
            let mut h = op.header(vfid)?;
            h.descriptor_raw = descriptor;
            op.put_header(vfid, &h)
        })
    }

    // ---- pages ----

    /// Engine `file_alloc` of a user page (`file_perm_alloc`): a page of the first partial sector of the header
    /// table, the file expanded when no page is free. The page is written with a header naming it, `ptype`, NULL
    /// LSA, and a zeroed body.
    pub fn alloc_page(&self, vfid: Vfid, ptype: PageType) -> io::Result<Vpid> {
        self.run(|op| alloc_user_page(op, vfid, ptype, false))
    }

    /// Like [`Self::alloc_page`] and records the page as the file's sticky first page (never deallocated; a heap's
    /// header page, a B-tree root, the tracker's item page). Refuses a file that already has one.
    pub fn alloc_sticky_first_page(&self, vfid: Vfid, ptype: PageType) -> io::Result<Vpid> {
        self.run(|op| alloc_user_page(op, vfid, ptype, true))
    }

    /// Engine `file_dealloc` of a user page (`file_perm_dealloc`, run at commit). Refuses the header page and the
    /// sticky first page. Sectors stay reserved by the file, as in the engine.
    pub fn dealloc_page(&self, vfid: Vfid, vpid: Vpid) -> io::Result<()> {
        self.run(|op| {
            let h = op.header(vfid)?;
            if vpid == vfid.header_vpid() || vpid == h.vpid_sticky_first || h.is_numerable() {
                return Err(err(
                    "dealloc_page: header, sticky or numerable-file page".into()
                ));
            }
            op.perm_dealloc(vfid, vpid, AllocType::UserPage)
        })
    }
}

fn alloc_user_page(
    op: &mut SpaceOp,
    vfid: Vfid,
    ptype: PageType,
    sticky: bool,
) -> io::Result<Vpid> {
    let h = op.header(vfid)?;
    if h.is_numerable() || h.is_temporary() {
        return Err(err(
            "alloc_page: numerable and temporary files are not supported".into(),
        ));
    }
    if sticky && !h.vpid_sticky_first.is_null() {
        return Err(err("file already has a sticky first page".into()));
    }
    let vpid = op.perm_alloc(vfid, AllocType::UserPage)?;
    if sticky {
        let mut h = op.header(vfid)?;
        h.vpid_sticky_first = vpid;
        op.put_header(vfid, &h)?;
    }
    let np = op.pages.blank(vpid);
    page::init_header(np, vpid, ptype);
    Ok(vpid)
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
