//! Non-leaf record: a child page and its separator key.
//!
//! Input/output: a record of a non-leaf page (slot ≥ 1).
//! Source: engine `btree_write_fixed_portion_of_non_leaf_record_to_orbuf`, `btree_write_record_ex` (btree.c),
//! `btree_connect_page` (btree_load.c), develop 5b1867174, same in release/11.4 46212adcd; R6 § 3.4.
//!
//! Layout (big-endian): child pageid i32 · child volid i16 · key_len i16 (-1 = overflow key) · key image
//! or overflow key VPID (6 B) · zero pad to 4.

use super::DISK_VPID_SIZE;
use super::be_field::{get_disk_vpid, pad_to_4, put_disk_vpid};
use super::leaf_record::KeyRef;
use crate::ids::Vpid;

#[derive(Clone, Copy, Debug)]
pub struct NonLeafRecord<'a> {
    pub child: Vpid,
    pub key: KeyRef<'a>,
}

pub fn read_nonleaf_record(b: &[u8]) -> Result<NonLeafRecord<'_>, &'static str> {
    if b.len() < 8 {
        return Err("non-leaf record shorter than its fixed part");
    }
    let child = get_disk_vpid(b, 0);
    let key_len = i16::from_be_bytes([b[6], b[7]]);
    let key = if key_len < 0 {
        if b.len() < 8 + DISK_VPID_SIZE {
            return Err("overflow key link runs past the record");
        }
        KeyRef::Overflow(get_disk_vpid(b, 8))
    } else {
        let end = 8 + key_len as usize;
        if end > b.len() {
            return Err("key image runs past the record");
        }
        KeyRef::Inline(&b[8..end])
    };
    Ok(NonLeafRecord { child, key })
}

pub fn write_nonleaf_record(out: &mut Vec<u8>, child: Vpid, key: KeyRef) {
    out.clear();
    put_disk_vpid(out, child);
    match key {
        KeyRef::Inline(img) => {
            out.extend_from_slice(&(img.len() as i16).to_be_bytes());
            out.extend_from_slice(img);
        }
        KeyRef::Overflow(v) => {
            out.extend_from_slice(&(-1i16).to_be_bytes());
            put_disk_vpid(out, v);
        }
    }
    pad_to_4(out);
}
