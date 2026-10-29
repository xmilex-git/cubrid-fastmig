//! Overflow key pages: a key image too long for a B-tree page, stored over a chain of pages of the
//! B-tree's overflow key file (`FILE_BTREE_OVERFLOW_KEY`, page type `PAGE_OVERFLOW`, not slotted).
//!
//! Input/output: the user area (`page::db_area`) of each page of the chain.
//! Source: engine `OVERFLOW_FIRST_PART`, `OVERFLOW_REST_PART` (overflow_file.h, unchanged 11.4..develop),
//! `bt_load_store_overflow_key` (btree_load.c, develop 5b1867174); R6 § 3.6.
//!
//! Layout (host order): first page `next_vpid` @0, `length` i32 @8 (whole key), data @12;
//! further pages `next_vpid` @0, data @8. The last page's `next_vpid` is NULL.

use crate::ids::Vpid;
use crate::le_field::{get_i32, put_i32};

pub const FIRST_DATA_OFFSET: usize = 12;
pub const REST_DATA_OFFSET: usize = 8;

/// Pages a key of `len` bytes takes.
pub fn pages_for(len: usize, db_pagesize: usize) -> usize {
    let first = db_pagesize - FIRST_DATA_OFFSET;
    if len <= first {
        1
    } else {
        1 + (len - first).div_ceil(db_pagesize - REST_DATA_OFFSET)
    }
}

/// Writes page `i` of the chain for `key` into `area` (zeroed by the caller); `next` is the following page
/// or NULL.
pub fn write_page(area: &mut [u8], key: &[u8], i: usize, next: Vpid) {
    let db = area.len();
    next.write(area, 0);
    let (start, off) = if i == 0 {
        put_i32(area, 8, key.len() as i32);
        (0, FIRST_DATA_OFFSET)
    } else {
        (
            (db - FIRST_DATA_OFFSET) + (i - 1) * (db - REST_DATA_OFFSET),
            REST_DATA_OFFSET,
        )
    };
    let n = (key.len() - start).min(db - off);
    area[off..off + n].copy_from_slice(&key[start..start + n]);
}

/// Reads a whole key: `page(vpid)` returns the user area of a page of the overflow key file.
pub fn read_key<'p>(
    first: Vpid,
    mut page: impl FnMut(Vpid) -> Option<&'p [u8]>,
) -> Result<Vec<u8>, &'static str> {
    let a = page(first).ok_or("overflow key page missing")?;
    let len = get_i32(a, 8);
    if len < 0 {
        return Err("negative overflow key length");
    }
    let len = len as usize;
    let mut key = Vec::with_capacity(len);
    let take = |a: &[u8], off: usize, key: &mut Vec<u8>| {
        let n = (len - key.len()).min(a.len() - off);
        key.extend_from_slice(&a[off..off + n]);
    };
    take(a, FIRST_DATA_OFFSET, &mut key);
    let mut next = Vpid::read(a, 0);
    while key.len() < len {
        if next.is_null() {
            return Err("overflow key chain ends early");
        }
        let a = page(next).ok_or("overflow key page missing")?;
        take(a, REST_DATA_OFFSET, &mut key);
        next = Vpid::read(a, 0);
    }
    Ok(key)
}
