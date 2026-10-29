//! The first log page id F of the target's new active log: the vacuum log block boundary at or after the page that
//! follows the source's last log page, so every page LSA kept from the source is below (F, 0).
//!
//! Input: the source log header `append_lsa.pageid` and the source `vacuum_log_block_npages` (N, from
//! `BOOT_DB_PARM`). Output: F = ceil((append_lsa.pageid + 1) / N) × N.
//! Source: decision G4 workspace#485 (start one past the source log end, rounded up to a vacuum block boundary;
//! starting at append_lsa.pageid + 1 makes vacuum and archive removal look for a log page that never existed);
//! ADR 0010.

/// F for a source whose log ends on page `src_append_pageid`.
pub fn first_log_pageid(src_append_pageid: i64, vacuum_log_block_npages: i64) -> i64 {
    assert!(src_append_pageid >= 0 && vacuum_log_block_npages > 0);
    let n = vacuum_log_block_npages as u64;
    ((src_append_pageid as u64 + 1).div_ceil(n) * n) as i64
}
