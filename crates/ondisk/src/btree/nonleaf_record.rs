//! Non-leaf record: a child page and its separator key.
//!
//! Input/output: a record of a non-leaf page (slot ≥ 1).
//! Source: engine `btree_write_fixed_portion_of_non_leaf_record_to_orbuf`, `btree_write_record_ex` (btree.c),
//! `btree_connect_page` (btree_load.c), develop 5b1867174, same in release/11.4 46212adcd; R6 § 3.4.
//!
//! Layout (big-endian): child pageid i32 · child volid i16 · key_len i16 (-1 = overflow key) · key image
//! or overflow key VPID (6 B) · zero pad to 4.

use super::be_field::{pad_to_4, put_disk_vpid, read_disk_vpid};
use super::leaf_record::KeyRef;
use crate::ids::Vpid;
use crate::value::OrReader;

#[derive(Clone, Copy, Debug)]
pub struct NonLeafRecord<'a> {
    pub child: Vpid,
    pub key: KeyRef<'a>,
}

pub fn read_nonleaf_record(b: &[u8]) -> Result<NonLeafRecord<'_>, &'static str> {
    let mut r = OrReader::new(b);
    let short = |_| "non-leaf record shorter than its fixed part";
    let child = read_disk_vpid(&mut r).map_err(short)?;
    let key_len = r.i16().map_err(short)?;
    let key = if key_len < 0 {
        KeyRef::Overflow(
            read_disk_vpid(&mut r).map_err(|_| "overflow key link runs past the record")?,
        )
    } else {
        KeyRef::Inline(
            r.bytes(key_len as usize)
                .map_err(|_| "key image runs past the record")?,
        )
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
