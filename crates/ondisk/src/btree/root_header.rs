//! B-tree root header: slot 0 of the root page (the B-tree file's sticky first page).
//!
//! Input/output: the root header record (88 B fixed part + packed key domain).
//! Source: engine `BTREE_ROOT_HEADER` (btree_load.h), `btree_pack_root_header` (btree_load.c); R6 § 3.2.
//! 11.4 → develop changed only off 68: `rev_level:16 | deduplicate_key_idx:16` → `int rev_level` (#8104).
//! 10.2..11.3 write an 80 B fixed part with 32-bit counts ([`RootHeader::read_layout`]).

use zerocopy::little_endian::{I32, I64, U64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use super::node_header::{NodeHeader, NodeHeaderBefore11_4Le, NodeHeaderLe};
use crate::ids::{Oid, OidLe, Vfid, VfidLe};
use crate::source_version::HeaderLayout;
use crate::value::{DecodeError, DecodeResult};

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

    /// Fixed part in a layout: 80 B before 11.4, 88 B since.
    pub fn fixed_size(layout: HeaderLayout) -> usize {
        match layout {
            HeaderLayout::Before11_4 => size_of::<RootHeaderBefore11_4Le>(),
            HeaderLayout::Since11_4 => Self::FIXED_SIZE,
        }
    }

    /// Decodes a root header record from a volume written in `layout`; an error when the record does not reach the
    /// key domain. 10.2..11.3: [`RootHeaderBefore11_4Le`] (workspace#529).
    pub fn read_layout(b: &[u8], layout: HeaderLayout) -> DecodeResult<Self> {
        if b.len() <= Self::fixed_size(layout) {
            return Err(DecodeError {
                at: b.len(),
                what: format!("B-tree root header of {} B", b.len()),
            });
        }
        if layout == HeaderLayout::Since11_4 {
            return Ok(Self::read(b));
        }
        let (h, packed_key_domain) =
            RootHeaderBefore11_4Le::ref_from_prefix(b).expect("length checked above");
        let high = h.counts_high.get();
        Ok(Self {
            node: NodeHeader::read_layout(b, layout)?,
            num_oids: wide_count(h.num_oids.get(), high, 2),
            num_nulls: wide_count(h.num_nulls.get(), high, 12),
            num_keys: wide_count(h.num_keys.get(), high, 22),
            topclass_oid: h.topclass_oid.get(),
            unique_pk: h.unique_pk.get(),
            off68: h.off68.get(),
            ovfid: h.ovfid.get(),
            creator_mvccid: h.creator_mvccid.get(),
            packed_key_domain: packed_key_domain.to_vec(),
        })
    }

    pub fn read(b: &[u8]) -> Self {
        let (h, packed_key_domain) =
            RootHeaderLe::ref_from_prefix(b).expect("record holds the root header");
        Self {
            node: (&h.node).into(),
            num_oids: h.num_oids.get(),
            num_nulls: h.num_nulls.get(),
            num_keys: h.num_keys.get(),
            topclass_oid: h.topclass_oid.get(),
            unique_pk: h.unique_pk.get(),
            off68: h.off68.get(),
            ovfid: h.ovfid.get(),
            creator_mvccid: h.creator_mvccid.get(),
            packed_key_domain: packed_key_domain.to_vec(),
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let fixed = RootHeaderLe {
            node: (&self.node).into(),
            num_oids: self.num_oids.into(),
            num_nulls: self.num_nulls.into(),
            num_keys: self.num_keys.into(),
            topclass_oid: self.topclass_oid.into(),
            unique_pk: self.unique_pk.into(),
            off68: self.off68.into(),
            ovfid: self.ovfid.into(),
            creator_mvccid: self.creator_mvccid.into(),
        };
        let mut b = fixed.as_bytes().to_vec();
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

/// A 10.2..11.3 unique count: the low 32 bits as stored, the high bits from the `_64` word's 10-bit field at `shift`
/// (signed bit-fields, low bits first: `over:2`, then one field per count; 11.3.6 `btree_get_unique_statistics`:
/// `testb $0x3,0x34`, `shl 0xa; sar 0x16`; btree.c:6378@release/11.3). `over` 0 means no count needs high bits.
fn wide_count(low: i32, high: i32, shift: u32) -> i64 {
    match high & 0b11 {
        0 => low as i64,
        _ => low as i64 | ((((high << (22 - shift)) >> 22) as i64) << 32),
    }
}

/// The fixed part of [`RootHeader`] as stored by 11.4 and develop; offsets on the [`RootHeader`] fields.
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct RootHeaderLe {
    node: NodeHeaderLe,
    num_oids: I64,
    num_nulls: I64,
    num_keys: I64,
    topclass_oid: OidLe,
    unique_pk: I32,
    off68: I32,
    ovfid: VfidLe,
    creator_mvccid: U64,
}

/// The fixed part of the root header as stored by 10.2..11.3 (`ptype /o` of release 11.2.9 and 11.3.6), 80 B.
#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct RootHeaderBefore11_4Le {
    /// @0
    node: NodeHeaderBefore11_4Le,
    /// @28, low 32 bits.
    num_oids: I32,
    /// @32, low 32 bits.
    num_nulls: I32,
    /// @36, low 32 bits.
    num_keys: I32,
    /// @40
    topclass_oid: OidLe,
    /// @48
    unique_pk: I32,
    /// @52, `_64`: the counts' high bits ([`wide_count`]).
    counts_high: I32,
    /// @56, the 11.4 @68 word (11.3 `rev_level:16 | deduplicate_key_idx:16`, 11.2 `int rev_level`).
    off68: I32,
    /// @60
    ovfid: VfidLe,
    pad: [u8; 4],
    /// @72
    creator_mvccid: U64,
}
const _: () = assert!(size_of::<RootHeaderLe>() == RootHeader::FIXED_SIZE);
const _: () = assert!(size_of::<RootHeaderBefore11_4Le>() == 80);
