//! A B-tree page being filled: its records and the slotted-page space they leave, then the IO page.
//!
//! Input: header record and records in slot order. Output: the IO page (FILEIO header with NULL LSA,
//! `PAGE_BTREE`, slotted user area).
//! Source: engine B-tree pages are `spage_initialize(UNANCHORED_KEEP_SEQUENCE, BTREE_MAX_ALIGN)`
//! (`btree.c:5343`, develop 5b1867174); space accounting is `spage_find_empty_slot` /
//! `spage_max_space_for_new_record`, the same rules `fastmig_ondisk::spage::SpageBuilder` writes.

use fastmig_ondisk::Vpid;
use fastmig_ondisk::btree::BTREE_MAX_ALIGN;
use fastmig_ondisk::page::{self, PageType};
use fastmig_ondisk::spage::{Anchor, RecordType, SPAGE_HEADER_SIZE, SPAGE_SLOT_SIZE, SpageBuilder};

fn align(n: usize) -> usize {
    n.div_ceil(BTREE_MAX_ALIGN) * BTREE_MAX_ALIGN
}

pub struct BtreePage {
    pub vpid: Vpid,
    /// Slot 0 first.
    pub records: Vec<Vec<u8>>,
    total_free: usize,
}

impl BtreePage {
    pub fn new(vpid: Vpid, db_pagesize: usize) -> Self {
        Self {
            vpid,
            records: Vec::new(),
            total_free: align(db_pagesize - SPAGE_HEADER_SIZE),
        }
    }

    /// `spage_max_space_for_new_record` of the page as it stands.
    pub fn max_space_for_new_record(&self) -> usize {
        let free = self.total_free as isize - SPAGE_SLOT_SIZE as isize;
        if free < SPAGE_SLOT_SIZE as isize {
            0
        } else {
            free as usize / BTREE_MAX_ALIGN * BTREE_MAX_ALIGN
        }
    }

    pub fn fits(&self, len: usize) -> bool {
        align(len) + SPAGE_SLOT_SIZE <= self.total_free
    }

    pub fn push(&mut self, rec: Vec<u8>) {
        assert!(self.fits(rec.len()), "record does not fit the page");
        self.total_free -= align(rec.len()) + SPAGE_SLOT_SIZE;
        self.records.push(rec);
    }

    /// Records after the header.
    pub fn key_records(&self) -> usize {
        self.records.len().saturating_sub(1)
    }

    pub fn to_io_page(&self, io_pagesize: usize) -> Vec<u8> {
        let mut pg = vec![0u8; io_pagesize];
        page::init_header(&mut pg, self.vpid, PageType::Btree);
        let mut sp = SpageBuilder::new(
            page::db_area_mut(&mut pg),
            Anchor::UnanchoredKeepSequence,
            BTREE_MAX_ALIGN,
            false,
        );
        for r in &self.records {
            sp.insert(RecordType::Home, r)
                .expect("accounted record must fit");
        }
        sp.finish();
        pg
    }
}
