//! A develop bestspace page: a heap page that holds bestspace entries instead of records (11.4 has none).
//!
//! Input: the page size, the heap's class OID, the page's chain links and its entries. Output: how many such pages a
//! heap has and how many entries each holds, or one page's user area written as `heap_create_bestspace` leaves it.
//! Source: develop @5b1867174 `src/storage/heap_file.c` `heap_create_bestspace` (:3896-4073; page count from the
//! largest shard count, slot 0 `HEAP_CHAIN` with `HEAP_PAGE_FLAG_BESTSPACE`, slot 1 the entry array as REC_HOME),
//! `heap_vpid_init_new` (:2892-2932; `spage_initialize (ANCHORED_DONT_REUSE_SLOTS, HEAP_MAX_ALIGN = 4,
//! SAFEGUARD_RVSPACE)`), `heap_load_bestspace` (shards read in entry order, 64 per shard);
//! `src/storage/bestspace.hpp` (`ENTRIES_PER_SHARD = 64`, `MAX_SHARD_PAGE_COUNT = 4`); `system_parameter.c`
//! `PRM_ID_BESTSPACE_SHARD_COUNT` (default 8, upper bound 28); R2 § 1.5.

use crate::Oid;
use crate::Vpid;
use crate::page::db_pagesize;
use crate::record::heap_chain::{FLAG_BESTSPACE, HeapChain};
use crate::record::heap_hdr_stats::{BestspaceEntry, MAX_SHARD_PAGES};
use crate::spage::{Anchor, RecordType, SPAGE_HEADER_SIZE, SPAGE_SLOT_SIZE, SpageBuilder};

/// develop `bestspace::ENTRIES_PER_SHARD`.
pub const ENTRIES_PER_SHARD: usize = 64;
/// Upper bound of `PRM_ID_BESTSPACE_SHARD_COUNT`: the pages are sized for it whatever the setting.
pub const MAX_SHARDS: usize = 28;
/// Default `PRM_ID_BESTSPACE_SHARD_COUNT`, the shard count of every heap but the root class's.
pub const DEFAULT_SHARDS: u64 = 8;
/// Shard count of the root class heap (`heap_build_bestspace`: `OID_IS_ROOTOID`).
pub const ROOT_SHARDS: u64 = 1;
/// Slot of the entry array (`HEAP_BESTSPACE_ENTRIES_SLOTID`).
pub const ENTRIES_SLOT: usize = 1;

/// Entries one bestspace page holds: (`spage_max_record_size` − aligned `HEAP_CHAIN` − one slot) / 8.
pub fn entries_per_page(io_pagesize: usize) -> usize {
    let max_record = db_pagesize(io_pagesize) - SPAGE_HEADER_SIZE - SPAGE_SLOT_SIZE;
    (max_record - HeapChain::SIZE - SPAGE_SLOT_SIZE) / BestspaceEntry::SIZE
}

/// Bestspace pages of every heap: 1 at 16 KiB, 2 at 8 KiB, 4 at 4 KiB.
pub fn page_count(io_pagesize: usize) -> usize {
    let n = (MAX_SHARDS * ENTRIES_PER_SHARD).div_ceil(entries_per_page(io_pagesize));
    assert!(
        n <= MAX_SHARD_PAGES,
        "bestspace pages {n} > {MAX_SHARD_PAGES}"
    );
    n
}

/// Entry count of bestspace page `i` (pages hold [`MAX_SHARDS`] × [`ENTRIES_PER_SHARD`] entries in order).
pub fn entries_in_page(io_pagesize: usize, i: usize) -> usize {
    let per = entries_per_page(io_pagesize);
    (MAX_SHARDS * ENTRIES_PER_SHARD - i * per).min(per)
}

/// Writes bestspace page `i`'s user area (`page::db_area_mut`): chain record (class, links, max MVCCID 0, flags
/// BESTSPACE) in slot 0 and [`entries_in_page`] entries in slot 1, the first `entries.len()` taken from `entries`,
/// the rest NULL.
pub fn write(
    area: &mut [u8],
    io_pagesize: usize,
    i: usize,
    class_oid: Oid,
    prev: Vpid,
    next: Vpid,
    entries: &[BestspaceEntry],
) {
    let n = entries_in_page(io_pagesize, i);
    assert!(entries.len() <= n, "more entries than the page holds");
    let mut chain = [0u8; HeapChain::SIZE];
    HeapChain {
        class_oid,
        prev_vpid: prev,
        next_vpid: next,
        max_mvccid: 0,
        flags: FLAG_BESTSPACE,
    }
    .write(&mut chain);
    let mut rec = vec![0u8; n * BestspaceEntry::SIZE];
    for j in 0..n {
        entries
            .get(j)
            .unwrap_or(&BestspaceEntry::NULL)
            .write(&mut rec, j * BestspaceEntry::SIZE);
    }
    let mut b = SpageBuilder::new(area, Anchor::AnchoredDontReuseSlots, 4, true);
    let s0 = b.insert(RecordType::Home, &chain);
    let s1 = b.insert(RecordType::Home, &rec);
    assert_eq!(
        (s0, s1),
        (Some(0), Some(ENTRIES_SLOT)),
        "bestspace page layout"
    );
    b.finish();
}
