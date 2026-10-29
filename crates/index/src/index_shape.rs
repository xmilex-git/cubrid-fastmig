//! What the builder must know about one index besides its rows: uniqueness, class, key domain, page size,
//! and the thresholds that follow from the page size.
//!
//! Source: R6 § 3.2 (root header fields), § 4.3–4.4 (fill thresholds); engine `LOAD_FIXED_EMPTY_FOR_LEAF`,
//! `LOAD_FIXED_EMPTY_FOR_NONLEAF`, `BTREE_MAX_OIDCOUNT_IN_LEAF_RECORD` (btree_load.h, develop 5b1867174),
//! `bt_unfill_factor` default 0.05 (system_parameter.c). Values are from `DB_PAGESIZE` (16344 B for a 16 KiB
//! IO page), not the IO page size R6 computed with.

use fastmig_ondisk::btree::{
    DISK_VPID_SIZE, PRIMARY_KEY_BIT, UNIQUE_BIT, max_keylen_inpage, max_oidlen_inpage,
    object_fixed_size,
};
use fastmig_ondisk::{Oid, db_pagesize};

/// Engine default of `bt_unfill_factor`.
pub const DEFAULT_UNFILL_FACTOR: f64 = 0.05;

#[derive(Clone, Debug)]
pub struct IndexShape {
    pub unique: bool,
    pub primary_key: bool,
    /// The indexed class (root header `topclass_oid`; class OID of unique objects).
    pub topclass: Oid,
    /// `or_put_domain(key_type)` in the target version's format (root header tail).
    pub packed_key_domain: Vec<u8>,
    pub io_pagesize: usize,
    pub unfill_factor: f64,
}

impl IndexShape {
    pub fn db_pagesize(&self) -> usize {
        db_pagesize(self.io_pagesize)
    }

    pub fn unique_pk(&self) -> i32 {
        (if self.unique { UNIQUE_BIT } else { 0 })
            | (if self.primary_key { PRIMARY_KEY_BIT } else { 0 })
    }

    /// A key image this long goes to the overflow key file.
    pub fn max_keylen_inpage(&self) -> usize {
        max_keylen_inpage(self.db_pagesize())
    }

    pub fn fixed_object_size(&self) -> usize {
        object_fixed_size(self.unique)
    }

    /// Objects one leaf record holds (`BTREE_MAX_OIDCOUNT_IN_LEAF_RECORD`): 85 non-unique, 63 unique at 16 KiB.
    pub fn max_objects_in_leaf_record(&self) -> usize {
        max_oidlen_inpage(self.db_pagesize()) / self.fixed_object_size()
    }

    /// `LOAD_FIXED_EMPTY_FOR_LEAF`: a leaf takes no further record that would leave less free space than this.
    pub fn leaf_reserve(&self) -> f64 {
        self.db_pagesize() as f64 * self.unfill_factor + DISK_VPID_SIZE as f64
    }

    /// `LOAD_FIXED_EMPTY_FOR_NONLEAF`.
    pub fn nonleaf_reserve(&self) -> f64 {
        self.db_pagesize() as f64 * self.unfill_factor.max(0.1) + DISK_VPID_SIZE as f64
    }
}
