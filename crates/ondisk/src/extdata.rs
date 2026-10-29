//! File extensible data: the page-chained array of fixed-size items behind file tables and the file tracker.
//!
//! Input: a page and the offset of one component in its DB page. Output: the component header and its items, the
//! chain of components across pages, or the component with items inserted or removed.
//! Source: engine `src/storage/file_manager.c:209-218` (`FILE_EXTENSIBLE_DATA`), `file_extdata_init` (:1468-1499),
//! `file_extdata_insert_at` / `file_extdata_remove_at`; same in release/11.4 @46212adcd and develop @5b1867174
//! (R5 § 2.5).

use std::io;

use crate::ids::Vpid;
use crate::le_field::{get_i16, put_i16};
use crate::page::IO_RESERVED_SIZE;
use crate::volume_set::VolumeSet;

/// Engine `FILE_EXTDATA_HEADER_ALIGNED_SIZE`: `vpid_next` @0, `max_size` i16 @8, `size_of_item` i16 @10,
/// `n_items` i16 @12; items start at 16.
pub const EXTDATA_HEADER_SIZE: usize = 16;

/// One component's header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtdataHeader {
    /// Next component (at DB page offset 0 of that page), NULL on the last.
    pub vpid_next: Vpid,
    /// Bytes available for items.
    pub max_size: i16,
    pub size_of_item: i16,
    pub n_items: i16,
}

impl ExtdataHeader {
    /// Reads the component at DB page offset `at` of `page` (whole IO page).
    pub fn read(page: &[u8], at: usize) -> Self {
        let o = IO_RESERVED_SIZE + at;
        ExtdataHeader {
            vpid_next: Vpid::read(page, o),
            max_size: get_i16(page, o + 8),
            size_of_item: get_i16(page, o + 10),
            n_items: get_i16(page, o + 12),
        }
    }

    pub fn write(&self, page: &mut [u8], at: usize) {
        let o = IO_RESERVED_SIZE + at;
        self.vpid_next.write(page, o);
        put_i16(page, o + 8, self.max_size);
        put_i16(page, o + 10, self.size_of_item);
        put_i16(page, o + 12, self.n_items);
        put_i16(page, o + 14, 0);
    }

    /// Items the component can hold.
    pub fn capacity(&self) -> usize {
        (self.max_size / self.size_of_item) as usize
    }

    /// Engine `file_extdata_init`: an empty component filling `size` bytes (header included) for `item_size` items.
    pub fn init(size: usize, item_size: usize) -> Self {
        let mut max = (size - EXTDATA_HEADER_SIZE) / item_size * item_size;
        if !max.is_multiple_of(8) {
            // the engine keeps max_size 8-aligned by dropping one item
            max = (max - item_size).div_ceil(8) * 8;
        }
        ExtdataHeader {
            vpid_next: Vpid::NULL,
            max_size: max as i16,
            size_of_item: item_size as i16,
            n_items: 0,
        }
    }

    /// Engine `file_extdata_max_size`: bytes the component occupies, header included.
    pub fn footprint(&self) -> usize {
        EXTDATA_HEADER_SIZE + self.max_size as usize
    }
}

/// Item `i` of the component at `at`, as bytes.
pub fn item(page: &[u8], at: usize, i: usize) -> &[u8] {
    let h = ExtdataHeader::read(page, at);
    let s = IO_RESERVED_SIZE + at + EXTDATA_HEADER_SIZE + i * h.size_of_item as usize;
    &page[s..s + h.size_of_item as usize]
}

pub fn item_mut(page: &mut [u8], at: usize, i: usize) -> &mut [u8] {
    let h = ExtdataHeader::read(page, at);
    let s = IO_RESERVED_SIZE + at + EXTDATA_HEADER_SIZE + i * h.size_of_item as usize;
    &mut page[s..s + h.size_of_item as usize]
}

/// Inserts `bytes` as item `i`, shifting later items up. False if the component is full.
pub fn insert_at(page: &mut [u8], at: usize, i: usize, bytes: &[u8]) -> bool {
    let mut h = ExtdataHeader::read(page, at);
    let n = h.n_items as usize;
    let sz = h.size_of_item as usize;
    assert_eq!(bytes.len(), sz);
    assert!(i <= n);
    if n >= h.capacity() {
        return false;
    }
    let base = IO_RESERVED_SIZE + at + EXTDATA_HEADER_SIZE;
    page.copy_within(base + i * sz..base + n * sz, base + (i + 1) * sz);
    page[base + i * sz..base + (i + 1) * sz].copy_from_slice(bytes);
    h.n_items += 1;
    h.write(page, at);
    true
}

/// Removes item `i`, shifting later items down; the freed tail is zeroed.
pub fn remove_at(page: &mut [u8], at: usize, i: usize) {
    let mut h = ExtdataHeader::read(page, at);
    let n = h.n_items as usize;
    let sz = h.size_of_item as usize;
    assert!(i < n);
    let base = IO_RESERVED_SIZE + at + EXTDATA_HEADER_SIZE;
    page.copy_within(base + (i + 1) * sz..base + n * sz, base + i * sz);
    page[base + (n - 1) * sz..base + n * sz].fill(0);
    h.n_items -= 1;
    h.write(page, at);
}

/// One component of a chain: the page it lives in and its DB page offset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Component {
    pub vpid: Vpid,
    pub at: usize,
}

/// Walks the chain starting at (`first`, `at`); later components sit at offset 0 of their pages. Errors on a
/// cycle or more than `limit` components.
pub fn chain(
    vols: &VolumeSet,
    first: Vpid,
    at: usize,
    limit: usize,
) -> io::Result<Vec<(Component, Box<[u8]>)>> {
    let mut out = Vec::new();
    let mut c = Component { vpid: first, at };
    loop {
        if out.len() >= limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "extensible data chain too long or cyclic",
            ));
        }
        let page = vols.read_page(c.vpid)?;
        let next = ExtdataHeader::read(&page, c.at).vpid_next;
        out.push((c, page));
        if next.is_null() {
            return Ok(out);
        }
        c = Component { vpid: next, at: 0 };
    }
}
