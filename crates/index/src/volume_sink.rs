//! Page sink over a file of the target database's volumes (workspace#490 `file_alloc::FilePages`).
//!
//! Input: a created file and the page type its pages get. Output: allocated pages, written in place.
//! Source: a B-tree root is the file's sticky first page, allocated with the file (`btree_file`, engine
//! `btree_create_file`), so this sink hands out the other pages only; overflow key pages are `PAGE_OVERFLOW`
//! (R6 § 3.6).

use crate::page_sink::PageSink;
use fastmig_ondisk::file_alloc::FilePages;
use fastmig_ondisk::page::PageType;
use fastmig_ondisk::{Vfid, Vpid};
use std::io;

pub struct VolumeFileSink<'s, 'v> {
    pub pages: FilePages<'s, 'v>,
    ptype: PageType,
}

impl<'s, 'v> VolumeFileSink<'s, 'v> {
    pub fn btree(pages: FilePages<'s, 'v>) -> Self {
        Self {
            pages,
            ptype: PageType::Btree,
        }
    }

    pub fn overflow_key(pages: FilePages<'s, 'v>) -> Self {
        Self {
            pages,
            ptype: PageType::Overflow,
        }
    }
}

impl PageSink for VolumeFileSink<'_, '_> {
    fn io_pagesize(&self) -> usize {
        self.pages.io_pagesize()
    }

    fn vfid(&self) -> Vfid {
        self.pages.vfid
    }

    fn alloc_page(&self) -> io::Result<Vpid> {
        self.pages.alloc_page(self.ptype)
    }

    fn write_page(&self, vpid: Vpid, page: &[u8]) -> io::Result<()> {
        self.pages.write_page(vpid, page)
    }
}
