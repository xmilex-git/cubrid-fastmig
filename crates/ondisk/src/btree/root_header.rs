//! B-tree root header: slot 0 of the root page (the B-tree file's sticky first page).
//!
//! Input/output: the root header record (88 B fixed part + packed key domain).
//! Source: engine `BTREE_ROOT_HEADER` (btree_load.h), `btree_pack_root_header` (btree_load.c); R6 § 3.2.
//! 11.4 → develop changed only off 68: `rev_level:16 | deduplicate_key_idx:16` → `int rev_level` (#8104).

use super::node_header::NodeHeader;
use crate::ids::{Oid, Vfid};
use crate::le_field::{get_i32, get_i64, get_u64, put_i32, put_i64, put_u64};

/// Engine `BTREE_ROOT_HEADER` (src/storage/btree_load.h), host order. Fixed part 88 B in 11.4 and develop
/// (release/11.4 46212adcd, develop 5b1867174); checked on T1 pristine clean root pages (unique statistics
/// match the counted leaf objects, workspace#493).
#[derive(Clone, Debug, PartialEq)]
pub struct RootHeader {
    /// @0 node header; `node_level` = tree height.
    pub node: NodeHeader,
    /// @32 `num_oids` i64: unique index rows (NULL keys included); -1 otherwise.
    pub num_oids: i64,
    /// @40 `num_nulls` i64: unique index rows with an all-NULL key; -1 otherwise.
    pub num_nulls: i64,
    /// @48 `num_keys` i64: unique index distinct keys; -1 otherwise.
    pub num_keys: i64,
    /// @56 `topclass_oid`: the indexed class.
    pub topclass_oid: Oid,
    /// @64 `unique_pk` i32: bit 0 unique, bit 1 primary key.
    pub unique_pk: i32,
    /// @68 i32. develop: `rev_level` (0). 11.4: low 16 bits `rev_level`, high 16 bits
    /// `deduplicate_key_idx` + 1 (0 = no hidden dedup column) — see `dedup_key_idx_114`.
    pub off68: i32,
    /// @72 `ovfid`: the overflow key file; NULL when no key overflowed.
    pub ovfid: Vfid,
    /// @80 `creator_mvccid` u64: 0 (`MVCCID_NULL`) for an offline build.
    pub creator_mvccid: u64,
    /// @88.. `or_put_domain(key_type)`: the key domain, in the format of the version that wrote it.
    pub packed_key_domain: Vec<u8>,
}

impl RootHeader {
    pub const FIXED_SIZE: usize = 88;

    pub fn read(b: &[u8]) -> Self {
        Self {
            node: NodeHeader::read(b),
            num_oids: get_i64(b, 32),
            num_nulls: get_i64(b, 40),
            num_keys: get_i64(b, 48),
            topclass_oid: Oid::read(b, 56),
            unique_pk: get_i32(b, 64),
            off68: get_i32(b, 68),
            ovfid: Vfid::read(b, 72),
            creator_mvccid: get_u64(b, 80),
            packed_key_domain: b[Self::FIXED_SIZE..].to_vec(),
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut b = vec![0u8; Self::FIXED_SIZE];
        self.node.write(&mut b);
        put_i64(&mut b, 32, self.num_oids);
        put_i64(&mut b, 40, self.num_nulls);
        put_i64(&mut b, 48, self.num_keys);
        self.topclass_oid.write(&mut b, 56);
        put_i32(&mut b, 64, self.unique_pk);
        put_i32(&mut b, 68, self.off68);
        self.ovfid.write(&mut b, 72);
        put_u64(&mut b, 80, self.creator_mvccid);
        b.extend_from_slice(&self.packed_key_domain);
        b
    }

    pub fn is_unique(&self) -> bool {
        self.unique_pk & super::UNIQUE_BIT != 0
    }

    /// 11.4 only: the hidden `_dedup_NN` column index of a DEDUPLICATE index, `None` when off.
    pub fn dedup_key_idx_114(&self) -> Option<i32> {
        let stored = (self.off68 as u32 >> 16) as i32;
        (stored != 0).then_some(stored - 1)
    }
}
