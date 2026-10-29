//! The chain record in slot 0 of every heap page: `HEAP_CHAIN` on data pages, the next-page link of the header page.
//!
//! Input: slot 0 of a heap page. Output: [`HeapChain`] (class OID, prev/next VPID, max MVCCID, flags), or the
//! 40 bytes written back; [`header_next_vpid`] for the header page's `HEAP_HDR_STATS.next_vpid`.
//! Source: engine `src/storage/heap_file.c` `struct heap_chain` and `HEAP_PAGE_FLAG_*` (develop @5b1867174
//! :226-272, release/11.4 @46212adcd :241-268); `HEAP_HDR_STATS` (`next_vpid` @16 in both, R2 § 1.4);
//! R2 § 1.3 (40 B, computed; same in both). Native (little-endian) layout.

use crate::Oid;
use crate::Vpid;
use zerocopy::little_endian::{U32, U64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use crate::ids::{OidLe, VpidLe};
use crate::record::heap_hdr_stats::HeapHdrStatsHeadLe;

/// Engine `HEAP_PAGE_FLAG_BESTSPACE` (develop only): the page holds bestspace entries, not records.
pub const FLAG_BESTSPACE: u32 = 0x1;
/// Engine `HEAP_PAGE_FLAG_NOT_IN_HEAP` (develop only).
pub const FLAG_NOT_IN_HEAP: u32 = 0x2;
/// Engine `HEAP_PAGE_FLAG_VACUUM_ONCE`: the vacuum status of a page one vacuum pass away from clean.
pub const FLAG_VACUUM_ONCE: u32 = 0x8000_0000;
/// Engine `HEAP_PAGE_FLAG_VACUUM_STATUS_MASK`: 0 none, `0x8000_0000` once, otherwise unknown.
pub const FLAG_VACUUM_STATUS_MASK: u32 = 0xC000_0000;

/// Engine `HEAP_CHAIN`, 40 B: `class_oid` @0, `prev_vpid` @8, `next_vpid` @16, `max_mvccid` u64 @24,
/// `flags` u32 @32, padding @36 (both versions; offsets computed, R2 § 1.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeapChain {
    pub class_oid: Oid,
    pub prev_vpid: Vpid,
    pub next_vpid: Vpid,
    pub max_mvccid: u64,
    pub flags: u32,
}

impl HeapChain {
    pub const SIZE: usize = 40;

    /// Decodes slot 0 of a data page; `None` if the record is not 40 bytes (then it is the header page's stats).
    pub fn read(rec: &[u8]) -> Option<HeapChain> {
        let c = HeapChainLe::ref_from_bytes(rec).ok()?;
        Some(HeapChain {
            class_oid: c.class_oid.get(),
            prev_vpid: c.prev_vpid.get(),
            next_vpid: c.next_vpid.get(),
            max_mvccid: c.max_mvccid.get(),
            flags: c.flags.get(),
        })
    }

    pub fn write(&self, rec: &mut [u8]) {
        let le = HeapChainLe {
            class_oid: self.class_oid.into(),
            prev_vpid: self.prev_vpid.into(),
            next_vpid: self.next_vpid.into(),
            max_mvccid: self.max_mvccid.into(),
            flags: self.flags.into(),
            pad: [0; 4],
        };
        le.write_to_prefix(rec).expect("slot 0 holds HEAP_CHAIN");
    }

    pub fn is_bestspace(&self) -> bool {
        self.flags & FLAG_BESTSPACE != 0
    }
}

/// The header page's link to the first data page: `HEAP_HDR_STATS.next_vpid` @16 (11.4 and develop).
pub fn header_next_vpid(stats: &[u8]) -> Vpid {
    HeapHdrStatsHeadLe::of(stats).next_vpid.get()
}

/// [`HeapChain`] as stored.
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct HeapChainLe {
    class_oid: OidLe,
    prev_vpid: VpidLe,
    next_vpid: VpidLe,
    max_mvccid: U64,
    flags: U32,
    pad: [u8; 4],
}
const _: () = assert!(size_of::<HeapChainLe>() == HeapChain::SIZE);
