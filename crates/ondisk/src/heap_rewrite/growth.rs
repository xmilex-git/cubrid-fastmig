//! An upper bound of the pages a heap's page treatment appends for its re-encoded records that grow (workspace#526):
//! each one counted whole as if it moved to a new page — a record of a quarter page or more as its own pages
//! (overflow pages plus one), the smaller ones packed three quarters full (a page takes another record of under a
//! quarter page while it is less full than that) — plus one partial page per heap.
//!
//! Input: per record its length before and after re-encoding. Output: [`Growth::pages`].
//! Source: `placement::place` (a grown record that does not fit its page moves), workspace#526.

use crate::db_pagesize;
use crate::spage::{SPAGE_HEADER_SIZE, SPAGE_SLOT_SIZE};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Growth {
    /// Bytes of the grown records under a quarter page, aligned, with their slot.
    pub small: u64,
    /// Pages of the grown records of a quarter page or more.
    pub big_pages: u64,
}

impl Growth {
    /// The bytes of a heap page records may take.
    pub fn usable(io_pagesize: usize) -> usize {
        db_pagesize(io_pagesize) - SPAGE_HEADER_SIZE - 64
    }

    /// One record of `old` bytes re-encoded to `new`.
    pub fn add(&mut self, old: usize, new: usize, usable: usize) {
        if new <= old {
            return;
        }
        if new >= usable / 4 {
            self.big_pages += new.div_ceil(usable) as u64 + 1;
        } else {
            self.small += (new.div_ceil(8) * 8 + SPAGE_SLOT_SIZE) as u64;
        }
    }

    pub fn merge(&mut self, o: &Growth) {
        self.small += o.small;
        self.big_pages += o.big_pages;
    }

    /// The pages of one heap.
    pub fn pages(&self, usable: usize) -> u64 {
        let packed = self.small.div_ceil((usable * 3 / 4) as u64);
        if self.big_pages + packed == 0 {
            0
        } else {
            self.big_pages + packed + 1
        }
    }
}
