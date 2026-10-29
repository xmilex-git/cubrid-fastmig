//! A permanent file's tables read as one: its sectors (partial with page bitmaps, full), its table pages, its user
//! pages, and a consistency check against the header counts.
//!
//! Input: the volume set and a VFID. Output: [`FileTable`] — the page set of the file, per sector, and the list
//! of header/table mismatches.
//! Source: engine `src/storage/file_manager.c` `file_create` (:3480-3810, table layout), `file_perm_alloc`,
//! `file_header_sanity_check` (:912-1025), `file_table_collect_ftab_pages`; `FILE_PARTIAL_SECTOR`
//! (file_manager.h:162-168), `FILE_USER_PAGE_MARK_DELETE_FLAG` (:401); same in release/11.4 @46212adcd and
//! develop @5b1867174 (R5 § 2.4-2.5, F1).

use std::collections::BTreeSet;
use std::io;

use crate::extdata::{self, ExtdataHeader};
use crate::file_header::FileHeader;
use zerocopy::little_endian::U64;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use crate::ids::{Vfid, Vpid, Vsid, VsidLe};
use crate::sector_table::SECTOR_NPAGES;
use crate::volume_set::VolumeSet;

/// Engine `FILE_USER_PAGE_MARK_DELETE_FLAG`: high bit of a user page table entry's pageid.
pub const USER_PAGE_MARK_DELETE_FLAG: u32 = 0x8000_0000;
/// Longest table chain accepted before the file is called corrupt.
const CHAIN_LIMIT: usize = 1 << 20;

/// One sector of a file with the pages it has allocated (bit i = page `sectid*64 + i`; full sectors are all ones).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileSector {
    pub vsid: Vsid,
    pub page_bitmap: u64,
    /// In the full table rather than the partial table.
    pub full: bool,
}

impl FileSector {
    pub fn first_page(&self) -> Vpid {
        Vpid::new(self.vsid.volid, self.vsid.sectid * SECTOR_NPAGES)
    }

    /// The allocated pages of this sector, ascending.
    pub fn pages(&self) -> impl Iterator<Item = Vpid> + '_ {
        let base = self.first_page();
        (0..64)
            .filter(move |i| self.page_bitmap >> i & 1 != 0)
            .map(move |i| Vpid::new(base.volid, base.pageid + i))
    }
}

/// Engine `FILE_PARTIAL_SECTOR`: an item of a file's partial sector table.
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
pub struct PartialSectorLe {
    /// @0
    pub vsid: VsidLe,
    /// @8, bit i = page `sectid*64 + i` allocated.
    pub page_bitmap: U64,
}

impl PartialSectorLe {
    pub fn new(vsid: Vsid, page_bitmap: u64) -> Self {
        PartialSectorLe {
            vsid: vsid.into(),
            page_bitmap: page_bitmap.into(),
        }
    }

    /// The item at the start of `item`.
    pub fn of(item: &[u8]) -> &Self {
        Self::ref_from_prefix(item).expect("a partial table item").0
    }

    pub fn of_mut(item: &mut [u8]) -> &mut Self {
        Self::mut_from_prefix(item).expect("a partial table item").0
    }
}

/// A user page table entry of a numerable file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UserPageEntry {
    pub vpid: Vpid,
    pub marked_deleted: bool,
}

/// Everything the file manager knows about one permanent file.
#[derive(Clone, Debug)]
pub struct FileTable {
    pub vfid: Vfid,
    pub header: FileHeader,
    /// Partial table entries in table order, then full table entries in table order.
    pub sectors: Vec<FileSector>,
    /// The header page and every table extension page (the `n_page_ftab` pages).
    pub ftab_pages: BTreeSet<Vpid>,
    /// Numerable files only: the user page table in allocation order.
    pub user_page_table: Option<Vec<UserPageEntry>>,
}

impl FileTable {
    /// Reads the header page and walks the partial, full and (numerable) user page tables. Temporary files are
    /// refused (none exist in a shut-down database).
    pub fn read(vols: &VolumeSet, vfid: Vfid) -> io::Result<Self> {
        let hp = vols.read_page(vfid.header_vpid())?;
        let header = FileHeader::read(&hp);
        if header.is_temporary() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "temporary file"));
        }
        let mut ftab_pages = BTreeSet::from([vfid.header_vpid()]);
        let mut sectors = Vec::new();
        let mut walk = |at: i16, sink: &mut dyn FnMut(&[u8])| -> io::Result<()> {
            for (c, page) in extdata::chain(vols, vfid.header_vpid(), at as usize, CHAIN_LIMIT)? {
                ftab_pages.insert(c.vpid);
                for i in 0..ExtdataHeader::read(&page, c.at).n_items as usize {
                    sink(extdata::item(&page, c.at, i));
                }
            }
            Ok(())
        };
        walk(header.offset_to_partial_ftab, &mut |it| {
            sectors.push(FileSector {
                vsid: PartialSectorLe::of(it).vsid.get(),
                page_bitmap: PartialSectorLe::of(it).page_bitmap.get(),
                full: false,
            })
        })?;
        walk(header.offset_to_full_ftab, &mut |it| {
            sectors.push(FileSector {
                vsid: Vsid::read(it, 0),
                page_bitmap: u64::MAX,
                full: true,
            })
        })?;
        let user_page_table = if header.is_numerable() {
            let mut t = Vec::new();
            walk(header.offset_to_user_page_ftab, &mut |it| {
                let mut vpid = Vpid::read(it, 0);
                let marked_deleted = vpid.pageid as u32 & USER_PAGE_MARK_DELETE_FLAG != 0;
                vpid.pageid = (vpid.pageid as u32 & !USER_PAGE_MARK_DELETE_FLAG) as i32;
                t.push(UserPageEntry {
                    vpid,
                    marked_deleted,
                })
            })?;
            Some(t)
        } else {
            None
        };
        Ok(FileTable {
            vfid,
            header,
            sectors,
            ftab_pages,
            user_page_table,
        })
    }

    /// Every allocated page (table pages included), ascending.
    pub fn allocated_pages(&self) -> BTreeSet<Vpid> {
        self.sectors.iter().flat_map(|s| s.pages()).collect()
    }

    /// The user pages (allocated minus table pages), ascending. For a numerable file this equals the user page
    /// table as a set.
    pub fn user_pages(&self) -> BTreeSet<Vpid> {
        let mut p = self.allocated_pages();
        p.retain(|v| !self.ftab_pages.contains(v));
        p
    }

    /// The user pages grouped by sector, sectors ascending: the unit of a sector-parallel scan.
    pub fn user_pages_by_sector(&self) -> Vec<(Vsid, Vec<Vpid>)> {
        let mut s: Vec<_> = self
            .sectors
            .iter()
            .map(|sec| {
                (
                    sec.vsid,
                    sec.pages()
                        .filter(|v| !self.ftab_pages.contains(v))
                        .collect::<Vec<_>>(),
                )
            })
            .filter(|(_, p)| !p.is_empty())
            .collect();
        s.sort_by_key(|(v, _)| *v);
        s
    }

    /// Mismatches between the header counts and the tables, in the spirit of the engine's
    /// `file_header_sanity_check`; empty when consistent.
    pub fn check(&self) -> Vec<String> {
        let h = &self.header;
        let mut bad = Vec::new();
        fn expect(bad: &mut Vec<String>, what: &str, got: i64, want: i64) {
            if got != want {
                bad.push(format!("{what}: tables say {got}, header says {want}"));
            }
        }
        let partial: Vec<_> = self.sectors.iter().filter(|s| !s.full).collect();
        let nfull = self.sectors.len() - partial.len();
        let allocated = self.allocated_pages().len() as i64;
        expect(
            &mut bad,
            "sectors",
            self.sectors.len() as i64,
            h.n_sector_total as i64,
        );
        expect(
            &mut bad,
            "partial sectors",
            partial.len() as i64,
            h.n_sector_partial as i64,
        );
        expect(
            &mut bad,
            "full sectors",
            nfull as i64,
            h.n_sector_full as i64,
        );
        expect(
            &mut bad,
            "empty sectors",
            partial.iter().filter(|s| s.page_bitmap == 0).count() as i64,
            h.n_sector_empty as i64,
        );
        expect(
            &mut bad,
            "total pages",
            self.sectors.len() as i64 * SECTOR_NPAGES as i64,
            h.n_page_total as i64,
        );
        expect(
            &mut bad,
            "ftab pages",
            self.ftab_pages.len() as i64,
            h.n_page_ftab as i64,
        );
        expect(
            &mut bad,
            "user pages",
            allocated - self.ftab_pages.len() as i64,
            h.n_page_user as i64,
        );
        expect(
            &mut bad,
            "free pages",
            h.n_page_total as i64 - allocated,
            h.n_page_free as i64,
        );
        if let Some(t) = &self.user_page_table {
            let set: BTreeSet<Vpid> = t.iter().map(|e| e.vpid).collect();
            if set != self.user_pages() {
                bad.push(format!(
                    "user page table ({} entries) != allocated user pages",
                    t.len()
                ));
            }
            expect(
                &mut bad,
                "marked deleted",
                t.iter().filter(|e| e.marked_deleted).count() as i64,
                h.n_page_mark_delete as i64,
            );
        }
        let mut seen = BTreeSet::new();
        for s in &self.sectors {
            if !seen.insert(s.vsid) {
                bad.push(format!(
                    "sector {}|{} listed twice",
                    s.vsid.volid, s.vsid.sectid
                ));
            }
        }
        if h.self_vfid != self.vfid {
            bad.push(format!("header self {:?} != {:?}", h.self_vfid, self.vfid));
        }
        let alloc = self.allocated_pages();
        for v in &self.ftab_pages {
            if !alloc.contains(v) {
                bad.push(format!(
                    "table page {}|{} not allocated in the file",
                    v.volid, v.pageid
                ));
            }
        }
        bad
    }
}
