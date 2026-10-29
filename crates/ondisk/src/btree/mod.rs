//! B-tree index pages: node and root headers, leaf and non-leaf records, overflow OID pages with their
//! OID directory, overflow key pages.
//!
//! Input/output: the bytes of one slotted-page record (or one overflow key page); the page itself is
//! `spage`. Header structs are host order (`memcpy` in the engine), record fields are big-endian (`OR_PUT_*`).
//! Source: R6 § 3 (workspace research/fastmig-r6-index-build); engine `btree_load.h`, `btree.c`,
//! `btree_load.c` (develop 5b1867174, release/11.4 46212adcd).

pub mod be_field;
pub mod key_image;
pub mod leaf_record;
pub mod node_header;
pub mod nonleaf_record;
pub mod ovf_key;
pub mod ovf_oid_page;
pub mod root_header;

/// Engine `BTREE_MAX_ALIGN` = `INT_ALIGNMENT`: alignment of every B-tree page and of record tails.
pub const BTREE_MAX_ALIGN: usize = 4;

/// Engine `BTREE_MAX_KEYLEN_INPAGE` = `DB_PAGESIZE / 8`: a key image this long or longer goes to an
/// overflow key file. 2043 B for a 16 KiB IO page (R6 computed 2048 from 16384; the user area is 16344).
pub const fn max_keylen_inpage(db_pagesize: usize) -> usize {
    db_pagesize / 8
}

/// Engine `BTREE_MAX_OIDLEN_INPAGE` = `DB_PAGESIZE / 8`: bytes of objects a leaf record may hold, counted
/// in fixed-size objects (`BTREE_MAX_OIDCOUNT_IN_LEAF_RECORD`).
pub const fn max_oidlen_inpage(db_pagesize: usize) -> usize {
    db_pagesize / 8
}

/// Engine `DISK_VPID_SIZE`: a VPID written as int pageid + short volid (the overflow key link).
pub const DISK_VPID_SIZE: usize = 6;
/// Engine `DISK_VPID_ALIGNED_SIZE`: the same plus 2 B padding (the overflow OID link at a leaf record's end).
pub const DISK_VPID_ALIGNED_SIZE: usize = 8;

/// Engine `MVCCID_ALL_VISIBLE`: insert MVCCID of a fixed-size object whose row every transaction sees.
pub const MVCCID_ALL_VISIBLE: u64 = 3;

/// Engine `BTREE_CONSTRAINT_UNIQUE` / `BTREE_CONSTRAINT_PRIMARY_KEY` bits of the root header `unique_pk`.
pub const UNIQUE_BIT: i32 = 0x1;
pub const PRIMARY_KEY_BIT: i32 = 0x2;

/// Engine `BTREE_OBJECT_FIXED_SIZE`: OID + class OID + insert and delete MVCCID for a unique index,
/// OID + both MVCCIDs otherwise.
pub const fn object_fixed_size(unique: bool) -> usize {
    if unique { 32 } else { 24 }
}
