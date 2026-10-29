//! A new catalog file (`FILE_CATALOG`): its pages built from the directory, class-info and disk-representation
//! records of every class, in the order and page format the engine's `catalog_add_representation` and
//! `catalog_add_class_info` leave them for a newly created class.
//!
//! Input: the records as [`CatalogItem`]s in write order and the page VPIDs. Output: whole IO pages and, per item,
//! the page and slot of its record (its first fragment). Building twice with records of the same lengths (placeholder
//! OIDs first, the real ones once the pages are allocated) gives the same positions.
//! Source: engine develop @5b1867174 `system_catalog.c` `catalog_create` (one file, sticky first page = `hpgid`),
//! `catalog_initialize_new_page` (`PAGE_CATALOG`, `ANCHORED_DONT_REUSE_SLOTS`, `MAX_ALIGNMENT` 8, header record in slot
//! 0), `catalog_find_optimal_page` (a page with more than the record plus `CATALOG_MAX_SLOT_ID_SIZE` free),
//! `catalog_insert_representation_item` (a directory record adds one to the page's `dir_count`),
//! `catalog_write_unwritten_portion` / `catalog_get_record_from_page` (a record longer than its page continues in
//! slot 1 of the overflow pages the page header links). Which page a record goes to is ours: the current page while
//! it fits, else a new one (workspace#504).

use std::io;

use crate::catalog_dir::{CATALOG_MAX_SLOT_ID_SIZE, CatalogPageHeader, ClsInfo, DiskRepr, ReprDir};
use crate::file_alloc::{SpaceManager, perm_tablespace};
use crate::file_header::{FILE_DESCRIPTORS_SIZE, FileType};
use crate::page::{self, PageType, db_area_mut};
use crate::spage::{Anchor, RecordType, SpageBuilder, SpageEditor};
use crate::volume_set::VolumeSet;
use crate::{Vfid, Vpid};

/// Engine `MAX_ALIGNMENT` (`DOUBLE_ALIGNMENT`).
const MAX_ALIGNMENT: usize = 8;

/// One record of the catalog file.
#[derive(Clone, Debug)]
pub enum CatalogItem {
    /// A class's directory record (counted in its page's `dir_count`).
    Dir(ReprDir),
    ClsInfo(ClsInfo),
    /// A disk representation, cut into fragments when no page holds it whole.
    DiskRepr(DiskRepr),
}

/// The pages of a catalog file.
pub struct CatalogPages {
    /// Whole IO pages, in file allocation order (the first is the sticky first page, `ctid.hpgid`).
    pub pages: Vec<Vec<u8>>,
    /// Per item: (page index, slot) of its record or first fragment.
    pub at: Vec<(usize, usize)>,
}

struct Page {
    buf: Vec<u8>,
    header: CatalogPageHeader,
}

struct Builder<'a> {
    io_pagesize: usize,
    vpids: &'a [Vpid],
    pages: Vec<Page>,
    /// The page that takes the next record; `None` after a record was cut into fragments.
    open: Option<usize>,
}

impl Builder<'_> {
    /// The VPID of page `i` (NULL while the pages are not allocated yet).
    fn vpid(&self, i: usize) -> Vpid {
        self.vpids.get(i).copied().unwrap_or(Vpid::NULL)
    }

    /// `catalog_initialize_new_page`: a new page with its header record; returns its index.
    fn new_page(&mut self, is_overflow: bool) -> usize {
        let i = self.pages.len();
        let mut buf = vec![0u8; self.io_pagesize];
        page::init_header(&mut buf, self.vpid(i), PageType::Catalog);
        let header = CatalogPageHeader {
            overflow: Vpid::NULL,
            dir_count: 0,
            is_overflow,
        };
        let mut b = SpageBuilder::new(
            db_area_mut(&mut buf),
            Anchor::AnchoredDontReuseSlots,
            MAX_ALIGNMENT,
            true,
        );
        b.insert(RecordType::Home, &header.encode());
        b.finish();
        self.pages.push(Page { buf, header });
        i
    }

    fn editor(&mut self, i: usize) -> SpageEditor<'_> {
        // our own page, built above: always a slotted page
        SpageEditor::new(db_area_mut(&mut self.pages[i].buf)).expect("catalog page")
    }

    /// Bytes a record may take on page `i` (`spage_max_space_for_new_record - CATALOG_MAX_SLOT_ID_SIZE`).
    fn room(&mut self, i: usize) -> usize {
        self.editor(i)
            .max_space_for_new_record()
            .saturating_sub(CATALOG_MAX_SLOT_ID_SIZE)
    }

    /// The room of an empty page.
    fn empty_room(&self) -> usize {
        let mut buf = vec![0u8; self.io_pagesize];
        let mut b = SpageBuilder::new(
            db_area_mut(&mut buf),
            Anchor::AnchoredDontReuseSlots,
            MAX_ALIGNMENT,
            true,
        );
        b.insert(RecordType::Home, &[0u8; CatalogPageHeader::SIZE]);
        b.max_space_for_new_record()
            .saturating_sub(CATALOG_MAX_SLOT_ID_SIZE)
    }

    fn insert(&mut self, i: usize, rec: &[u8]) -> usize {
        // callers check the room first
        self.editor(i)
            .insert(RecordType::Home, rec)
            .expect("catalog record fits its page")
    }

    /// The page for a whole record of `len` bytes: the open page when it fits, else a new one.
    fn page_for(&mut self, len: usize) -> usize {
        let open = self.open;
        match open {
            Some(i) if self.room(i) >= len => i,
            _ => {
                let i = self.new_page(false);
                self.open = Some(i);
                i
            }
        }
    }

    fn put(&mut self, item: &CatalogItem) -> (usize, usize) {
        match item {
            CatalogItem::Dir(d) => {
                let rec = d.encode();
                let i = self.page_for(rec.len());
                self.pages[i].header.dir_count += 1;
                (i, self.insert(i, &rec))
            }
            CatalogItem::ClsInfo(c) => {
                let rec = c.encode_develop();
                let i = self.page_for(rec.len());
                (i, self.insert(i, &rec))
            }
            CatalogItem::DiskRepr(r) => {
                let empty = self.empty_room();
                let whole = r.encode_develop(usize::MAX, usize::MAX);
                if whole.len() == 1 && whole[0].len() <= empty {
                    let i = self.page_for(whole[0].len());
                    return (i, self.insert(i, &whole[0]));
                }
                // longer than a page: a page of its own, then overflow pages (slot 1 each)
                let first = self.new_page(false);
                let frags = r.encode_develop(self.room(first), empty);
                let slot = self.insert(first, &frags[0]);
                let mut prev = first;
                for f in &frags[1..] {
                    let i = self.new_page(true);
                    self.insert(i, f);
                    self.pages[prev].header.overflow = self.vpid(i);
                    prev = i;
                }
                self.open = None;
                (first, slot)
            }
        }
    }
}

/// Builds the pages of a catalog file holding `items` in this order; `vpids` names the pages (shorter than the
/// page count while sizing: missing VPIDs are written as NULL).
pub fn build(items: &[CatalogItem], io_pagesize: usize, vpids: &[Vpid]) -> CatalogPages {
    let mut b = Builder {
        io_pagesize,
        vpids,
        pages: Vec::new(),
        open: None,
    };
    let at = items.iter().map(|it| b.put(it)).collect();
    // the overflow links are known only now; a header record keeps its length
    for i in 0..b.pages.len() {
        let rec = b.pages[i].header.encode();
        b.editor(i)
            .update(0, &rec)
            .expect("catalog page header keeps its length");
    }
    if b.pages.is_empty() {
        b.new_page(false);
    }
    CatalogPages {
        pages: b.pages.into_iter().map(|p| p.buf).collect(),
        at,
    }
}

/// Engine `catalog_create` without the extendible hash (develop never reads `ctid.xhid`, workspace#492): a
/// `FILE_CATALOG` file of `npages` pages, the first its sticky first page. Returns the file and its pages.
pub fn create_file(space: &SpaceManager, npages: usize) -> io::Result<(Vfid, Vec<Vpid>)> {
    let vfid = space.create_file(
        FileType::Catalog,
        [0u8; FILE_DESCRIPTORS_SIZE],
        perm_tablespace(space.volumes().io_pagesize(), 1),
    )?;
    let mut vpids = vec![space.alloc_sticky_first_page(vfid, PageType::Catalog)?];
    for _ in 1..npages.max(1) {
        vpids.push(space.alloc_page(vfid, PageType::Catalog)?);
    }
    Ok((vfid, vpids))
}

/// Writes pages built with `vpids` to those pages.
pub fn write(vols: &VolumeSet, pages: &CatalogPages, vpids: &[Vpid]) -> io::Result<()> {
    for (p, &v) in pages.pages.iter().zip(vpids) {
        vols.write_page(v, p)?;
    }
    Ok(())
}
