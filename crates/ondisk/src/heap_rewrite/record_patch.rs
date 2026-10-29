//! Vacuum-residue removal from one record's MVCC header: the cleaned record, and the fixed-size header of a big
//! record's first overflow page.
//!
//! Input: a whole record (or the DB area of a first overflow page). Output: the record without insert MVCCID and
//! previous-version LSA, or the overflow header reset in place.
//! Source: engine `src/query/vacuum.c` `vacuum_heap_record_insid_and_prev_version` (develop @5b1867174
//! :2294-2335; the delete MVCCID, if present, stays), `vacuum_heap_record` big-record branch (:2266-2293),
//! `heap_set_mvcc_rec_header_on_overflow` (`heap_file.c` :19750-19780; overflow header always 32 B, flags 7),
//! `MVCCID_ALL_VISIBLE` = 3 (`storage_common.h`), NULL LSA = all bits set (`log_lsa.hpp`); same in release/11.4
//! @46212adcd. R2 § 3 R3. The tool also drops a previous-version LSA left alone (R2 § 3 R3: it points into the
//! discarded log).

use crate::record::mvcc_header::{FLAG_DELID, FLAG_INSID, FLAG_PREV_VERSION, RecordHeader};
use crate::value::{DecodeError, DecodeResult, OrWriter};

/// Engine `MVCCID_ALL_VISIBLE`.
pub const MVCCID_ALL_VISIBLE: u64 = 3;
/// Engine `OR_MVCC_MAX_HEADER_SIZE`: repid word, CHN, INSID, DELID, previous-version LSA.
pub const OVERFLOW_HEADER_SIZE: usize = 32;
/// `OVERFLOW_FIRST_PART.data`: where the record starts in the first overflow page's DB area.
pub const OVERFLOW_FIRST_DATA: usize = 12;

/// The record with `header` replacing its current header.
pub fn with_header(rec: &[u8], old: &RecordHeader, header: &RecordHeader) -> Vec<u8> {
    let mut w = OrWriter::new();
    header.write(&mut w);
    w.bytes(&rec[old.size()..]);
    w.buf
}

/// `rec` without INSID and previous-version LSA; `None` when it has neither (nothing to rewrite).
pub fn strip_insid_and_prev(rec: &[u8]) -> DecodeResult<Option<Vec<u8>>> {
    let h = RecordHeader::read(rec)?;
    if h.insid.is_none() && h.prev_version_lsa.is_none() {
        return Ok(None);
    }
    let mut clean = h;
    clean.insid = None;
    clean.prev_version_lsa = None;
    Ok(Some(with_header(rec, &h, &clean)))
}

/// The 32-byte header a big MVCC record keeps on its first overflow page with no residue: INSID = ALL_VISIBLE,
/// DELID = 0, previous-version LSA NULL, all three flags set; repid, bound-bit and offset-size bits and CHN kept.
pub fn residue_free_overflow_header(h: &RecordHeader) -> RecordHeader {
    let mut c = *h;
    c.mvcc_flags |= FLAG_INSID | FLAG_DELID | FLAG_PREV_VERSION;
    c.insid = Some(MVCCID_ALL_VISIBLE);
    c.delid = Some(0);
    c.prev_version_lsa = Some([0xFF; 8]);
    c
}

/// Resets the record header in a first overflow page's DB area (`area`) to [`residue_free_overflow_header`].
/// Refuses a header that is not the 32-byte overflow form.
pub fn reset_overflow_header(area: &mut [u8]) -> DecodeResult<()> {
    let at = OVERFLOW_FIRST_DATA;
    let h = RecordHeader::read(&area[at..])?;
    if h.size() != OVERFLOW_HEADER_SIZE {
        return Err(DecodeError {
            at,
            what: format!("overflow record header of {} B, not 32", h.size()),
        });
    }
    let mut w = OrWriter::new();
    residue_free_overflow_header(&h).write(&mut w);
    area[at..at + OVERFLOW_HEADER_SIZE].copy_from_slice(&w.buf);
    Ok(())
}
