//! The page header of an overflow chain page: what heap overflow records and B-tree overflow keys both use.
//!
//! Input/output: the user area (`page::db_area`) of a page of an overflow chain.
//! Source: engine `OVERFLOW_FIRST_PART`, `OVERFLOW_REST_PART` (src/storage/overflow_file.h, unchanged 11.4..develop:
//! first page `next_vpid` @0, `length` i32 @8 (the whole value), data @12; further pages `next_vpid` @0, data @8;
//! the last page's `next_vpid` is NULL); R2 § 1.8, R6 § 3.6.

use zerocopy::little_endian::I32;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use crate::ids::VpidLe;

/// Engine `OVERFLOW_FIRST_PART` before its data.
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
pub struct OverflowFirstPart {
    pub next_vpid: VpidLe,
    /// Length of the whole value the chain carries.
    pub length: I32,
}

/// Engine `OVERFLOW_REST_PART` before its data.
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
pub struct OverflowRestPart {
    pub next_vpid: VpidLe,
}

/// Where the data of the first page starts.
pub const FIRST_DATA_OFFSET: usize = size_of::<OverflowFirstPart>();
/// Where the data of every further page starts.
pub const REST_DATA_OFFSET: usize = size_of::<OverflowRestPart>();

impl OverflowFirstPart {
    /// The header of the first page of a chain.
    pub fn of(area: &[u8]) -> &Self {
        Self::ref_from_prefix(area)
            .expect("a page holds the overflow header")
            .0
    }

    pub fn of_mut(area: &mut [u8]) -> &mut Self {
        Self::mut_from_prefix(area)
            .expect("a page holds the overflow header")
            .0
    }
}

impl OverflowRestPart {
    /// The header of a further page of a chain (also the `next_vpid` of the first page).
    pub fn of(area: &[u8]) -> &Self {
        Self::ref_from_prefix(area)
            .expect("a page holds the overflow header")
            .0
    }

    pub fn of_mut(area: &mut [u8]) -> &mut Self {
        Self::mut_from_prefix(area)
            .expect("a page holds the overflow header")
            .0
    }
}

/// A chain just written: its first page and how many pages it took.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverflowChain {
    pub first: crate::Vpid,
    pub pages: usize,
}
