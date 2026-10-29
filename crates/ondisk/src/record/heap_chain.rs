//! The chain record in slot 0 of every heap page: `HEAP_CHAIN` on data pages, the next-page link of the header page.
//!
//! Input: slot 0 of a heap page. Output: [`HeapChain`] (class OID, prev/next VPID, max MVCCID, flags), or the
//! 40 bytes written back; [`header_next_vpid`] for the header page's `HEAP_HDR_STATS.next_vpid`.
//! Source: engine `src/storage/heap_file.c` `struct heap_chain` and `HEAP_PAGE_FLAG_*` (develop @5b1867174
//! :226-272, release/11.4 @46212adcd :241-268); `HEAP_HDR_STATS` (`next_vpid` @16 in both, R2 § 1.4);
//! R2 § 1.3 (40 B, computed; same in both). Native (little-endian) layout.

use crate::Oid;
use crate::Vpid;
use crate::le_field::{get_u32, get_u64, put_u32, put_u64};

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
        (rec.len() == Self::SIZE).then(|| HeapChain {
            class_oid: Oid::read(rec, 0),
            prev_vpid: Vpid::read(rec, 8),
            next_vpid: Vpid::read(rec, 16),
            max_mvccid: get_u64(rec, 24),
            flags: get_u32(rec, 32),
        })
    }

    pub fn write(&self, rec: &mut [u8]) {
        self.class_oid.write(rec, 0);
        self.prev_vpid.write(rec, 8);
        self.next_vpid.write(rec, 16);
        put_u64(rec, 24, self.max_mvccid);
        put_u32(rec, 32, self.flags);
        put_u32(rec, 36, 0);
    }

    pub fn is_bestspace(&self) -> bool {
        self.flags & FLAG_BESTSPACE != 0
    }
}

/// The header page's link to the first data page: `HEAP_HDR_STATS.next_vpid` @16 (11.4 and develop).
pub fn header_next_vpid(stats: &[u8]) -> Vpid {
    Vpid::read(stats, 16)
}
