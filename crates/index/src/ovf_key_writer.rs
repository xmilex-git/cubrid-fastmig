//! Writes a key image too long for a B-tree page into the overflow key file.
//!
//! Input: the key image and the overflow key file's sink. Output: the chain's first VPID (the 6 B key link).
//! Source: engine `bt_load_store_overflow_key` (btree_load.c, develop 5b1867174): pages claimed first, then
//! filled in chain order; R6 § 3.6. Pages are `PAGE_OVERFLOW`, not slotted.

use crate::page_sink::PageSink;
use fastmig_ondisk::btree::ovf_key;
use fastmig_ondisk::page::{self, PageType};
use fastmig_ondisk::{Vpid, db_pagesize};
use std::io;

pub fn write_overflow_key(sink: &dyn PageSink, key: &[u8]) -> io::Result<Vpid> {
    let io_size = sink.io_pagesize();
    let n = ovf_key::pages_for(key.len(), db_pagesize(io_size));
    let vpids = (0..n)
        .map(|_| sink.alloc_page())
        .collect::<io::Result<Vec<_>>>()?;
    for (i, &v) in vpids.iter().enumerate() {
        let mut pg = vec![0u8; io_size];
        page::init_header(&mut pg, v, PageType::Overflow);
        let next = vpids.get(i + 1).copied().unwrap_or(Vpid::NULL);
        ovf_key::write_page(page::db_area_mut(&mut pg), key, i, next);
        sink.write_page(v, &pg)?;
    }
    Ok(vpids[0])
}
