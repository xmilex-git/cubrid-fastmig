//! Log page framing shared by every active-log page: the page header (`LOG_HDRPAGE`), the page checksum, and the
//! log record header used for the end-of-log record of a new log.
//!
//! Input: one log page (`db_logpagesize` bytes). Output: its header fields, or the page with header and checksum set.
//! Source: engine `src/transaction/log_storage.hpp:62-71` (`LOG_HDRPAGE`), `log_page_buffer.c:455-494`
//! (`logpb_compute_page_checksum`), `log_record.hpp:108,145-153` (`LOG_RECORD_HEADER`, `LOG_END_OF_LOG` 35);
//! same in release/11.4 @46212adcd and develop @5b1867174; R5 § 2.7. Checksum checked on the T1 `_lgat` (workspace#490).

use crate::crc32::crc32;
use zerocopy::little_endian::{I16, I32, I64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use crate::lsa::{Lsa, LsaLe};

/// `sizeof (LOG_HDRPAGE)`; the page area (`LOG_PAGE::area`) starts here.
pub const LOG_PAGE_HEADER_SIZE: usize = 16;
/// Logical page id of the active log's header page (engine `LOGPB_HEADER_PAGE_ID`).
pub const LOG_HEADER_PAGE_ID: i64 = -9;
/// Engine `NULL_OFFSET` for `LOG_HDRPAGE::offset`: no record starts in this page.
pub const NULL_OFFSET: i16 = -1;
/// Engine `LOG_END_OF_LOG`.
pub const LOG_END_OF_LOG: i32 = 35;
/// `sizeof (LOG_RECORD_HEADER)`.
pub const LOG_RECORD_HEADER_SIZE: usize = 32;

/// Engine `LOG_HDRPAGE`: `logical_pageid` i64 @0, `offset` i16 @8, `flags` i16 @10, `checksum` i32 @12.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogPageHeader {
    pub logical_pageid: i64,
    pub offset: i16,
    pub flags: i16,
    pub checksum: i32,
}

/// Engine `LOG_HDRPAGE` as stored at the start of every log page.
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct LogPageHeaderLe {
    logical_pageid: I64,
    offset: I16,
    flags: I16,
    checksum: I32,
}
const _: () = assert!(size_of::<LogPageHeaderLe>() == LOG_PAGE_HEADER_SIZE);

fn page_header(page: &[u8]) -> &LogPageHeaderLe {
    LogPageHeaderLe::ref_from_prefix(page)
        .expect("a log page holds its header")
        .0
}

fn page_header_mut(page: &mut [u8]) -> &mut LogPageHeaderLe {
    LogPageHeaderLe::mut_from_prefix(page)
        .expect("a log page holds its header")
        .0
}

impl LogPageHeader {
    pub fn read(page: &[u8]) -> Self {
        let h = page_header(page);
        LogPageHeader {
            logical_pageid: h.logical_pageid.get(),
            offset: h.offset.get(),
            flags: h.flags.get(),
            checksum: h.checksum.get(),
        }
    }

    /// Writes logical page id, offset and flags; the checksum is [`set_checksum`]'s.
    pub fn write(&self, page: &mut [u8]) {
        let h = page_header_mut(page);
        h.logical_pageid.set(self.logical_pageid);
        h.offset.set(self.offset);
        h.flags.set(self.flags);
        h.checksum.set(self.checksum);
    }
}

/// Engine `logpb_compute_page_checksum`: CRC-32 over the first and last 16 bytes of every 4096-byte block, taken in
/// order with the checksum field read as 0.
pub fn compute_checksum(page: &[u8]) -> i32 {
    const BLOCK: usize = 4096;
    const SAMPLE: usize = 16;
    let mut buf = Vec::with_capacity(page.len() / BLOCK * SAMPLE * 2);
    for b in page.as_chunks::<BLOCK>().0 {
        buf.extend_from_slice(&b[..SAMPLE]);
        buf.extend_from_slice(&b[BLOCK - SAMPLE..]);
    }
    buf[12..16].fill(0);
    crc32(&buf) as i32
}

pub fn set_checksum(page: &mut [u8]) {
    let c = compute_checksum(page);
    page_header_mut(page).checksum.set(c);
}

pub fn checksum_ok(page: &[u8]) -> bool {
    compute_checksum(page) == page_header(page).checksum.get()
}

/// Engine `LOG_RECORD_HEADER`: `prev_tranlsa` @0, `back_lsa` @8, `forw_lsa` @16, `trid` i32 @24, `type` i32 @28.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogRecordHeader {
    pub prev_tranlsa: Lsa,
    pub back_lsa: Lsa,
    pub forw_lsa: Lsa,
    pub trid: i32,
    pub rectype: i32,
}

impl LogRecordHeader {
    /// The record a new log starts with: all LSAs NULL, `trid` 1 (`LOG_SYSTEM_TRANID`+1 as copydb writes it),
    /// type `LOG_END_OF_LOG` (log_page_buffer.c:10205-10230).
    pub fn end_of_log() -> Self {
        LogRecordHeader {
            prev_tranlsa: Lsa::NULL,
            back_lsa: Lsa::NULL,
            forw_lsa: Lsa::NULL,
            trid: 1,
            rectype: LOG_END_OF_LOG,
        }
    }

    pub fn read(buf: &[u8], off: usize) -> Self {
        let h = LogRecordHeaderLe::ref_from_prefix(&buf[off..])
            .expect("a log page holds the record header")
            .0;
        LogRecordHeader {
            prev_tranlsa: h.prev_tranlsa.get(),
            back_lsa: h.back_lsa.get(),
            forw_lsa: h.forw_lsa.get(),
            trid: h.trid.get(),
            rectype: h.rectype.get(),
        }
    }

    pub fn write(&self, buf: &mut [u8], off: usize) {
        let le = LogRecordHeaderLe {
            prev_tranlsa: self.prev_tranlsa.into(),
            back_lsa: self.back_lsa.into(),
            forw_lsa: self.forw_lsa.into(),
            trid: self.trid.into(),
            rectype: self.rectype.into(),
        };
        le.write_to_prefix(&mut buf[off..])
            .expect("a log page holds the record header");
    }
}

/// Engine `LOG_RECORD_HEADER` as stored.
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct LogRecordHeaderLe {
    prev_tranlsa: LsaLe,
    back_lsa: LsaLe,
    forw_lsa: LsaLe,
    trid: I32,
    rectype: I32,
}
const _: () = assert!(size_of::<LogRecordHeaderLe>() == LOG_RECORD_HEADER_SIZE);
