//! Writes the target's new active log `<db>_lgat`: physical page 0 holds the log header, physical page 1 the
//! end-of-log record at logical page F; every other page is a hole (sparse file), as copydb leaves it.
//!
//! Input: the source log header, the target release line, F ([`crate::new_log_start`]) and the target name.
//! Output: the new file. Header fields follow copydb's new log (`logpb_initialize_header` + `was_copied`, source
//! `mvcc_next_id`), except that the log starts at F and `db_creation` stays the source's, so volume headers and
//! page LSAs are not written.
//! Source: engine `src/transaction/log_page_buffer.c:logpb_copy_database`, `logpb_initialize_header`
//! (develop @5b1867174); R5 § 2.7 (rules L1-L5); decision G4 workspace#485 and its prototype
//! `prototype/fastmig-g4-485/fm485_proto.py` (mklog), which booted develop release; ADR 0010.

use std::fs::OpenOptions;
use std::io;
use std::os::unix::fs::FileExt;
use std::path::Path;

use fastmig_ondisk::log_header::{LogHeader, MAGIC};
use fastmig_ondisk::log_page::{
    LOG_HEADER_PAGE_ID, LOG_PAGE_HEADER_SIZE, LogPageHeader, LogRecordHeader, NULL_OFFSET,
    set_checksum,
};
use fastmig_ondisk::{Lsa, VersionLine};

/// Engine `MVCCID_FIRST`: `oldest_visible_mvccid` of a new log (`logpb_vacuum_reset_log_header_cache`).
const MVCCID_FIRST: u64 = 4;

/// The header of the new log.
pub fn new_header(
    src: &LogHeader,
    target: VersionLine,
    first_pageid: i64,
    target_name: &str,
) -> LogHeader {
    let start = Lsa::new(first_pageid, 0);
    let mut hdr = LogHeader {
        magic: MAGIC.to_string(),
        cdc_arv_num_to_keep: 0,
        // the volumes keep their db_creation, so the log keeps the source's (workspace#485)
        db_creation: src.db_creation,
        vol_creation: src.vol_creation,
        db_release: target.db_release().to_string(),
        db_compatibility: target.db_compatibility(),
        db_iopagesize: src.db_iopagesize,
        db_logpagesize: src.db_logpagesize,
        is_shutdown: true,
        next_trid: 1,
        // never below the source: kept heap pages keep their HEAP_CHAIN max_mvccid, which must precede every new
        // MVCCID (heap_file.c:25045 MVCC_ID_PRECEDES (chain->max_mvccid, mvccid); workspace#499)
        mvcc_next_id: src.mvcc_next_id,
        avg_ntrans: 100,
        avg_nlocks: 977,
        npages: src.npages,
        db_charset: src.db_charset,
        was_copied: true,
        sysmeta_version: target.sysmeta_version(),
        fpageid: first_pageid,
        append_lsa: start,
        chkpt_lsa: start,
        nxarv_pageid: first_pageid,
        nxarv_phy_pageid: 1,
        nxarv_num: 0,
        last_arv_num_for_syscrashes: -1,
        last_deleted_arv_num: -1,
        bkup_level_lsa: [Lsa::NULL; 3],
        prefix_name: target_name.to_string(),
        has_logging_been_skipped: false,
        vacuum_last_blockid: 0,
        perm_status_obsolete: 0,
        bkinfo: src.bkinfo, // zeroed below, as logpb_initialize_backup_info leaves it
        ha_server_state: 0,
        ha_file_status: -1,
        eof_lsa: Lsa::NULL,
        smallest_lsa_at_last_chkpt: Lsa::NULL,
        mvcc_op_log_lsa: Lsa::NULL,
        oldest_visible_mvccid: MVCCID_FIRST,
        newest_block_mvccid: 0,
        ha_promotion_time: 0,
        db_restore_time: 0,
        mark_will_del: false,
        does_block_need_vacuum: false,
        was_active_log_reset: false,
    };
    hdr.bkinfo.fill(0);
    hdr
}

/// Creates `path` (must not exist) holding `hdr`: `npages + 1` log pages, pages 0 and 1 written.
pub fn write(path: &Path, hdr: &LogHeader) -> io::Result<()> {
    let lps = hdr.db_logpagesize as usize;
    let mut p0 = vec![0u8; lps];
    page_header(LOG_HEADER_PAGE_ID).write(&mut p0);
    hdr.write(&mut p0);
    set_checksum(&mut p0);

    let mut p1 = vec![0u8; lps];
    page_header(hdr.fpageid).write(&mut p1);
    LogRecordHeader::end_of_log().write(&mut p1, LOG_PAGE_HEADER_SIZE);
    set_checksum(&mut p1);

    let f = OpenOptions::new().write(true).create_new(true).open(path)?;
    f.set_len((hdr.npages as u64 + 1) * lps as u64)?;
    f.write_all_at(&p0, 0)?;
    f.write_all_at(&p1, lps as u64)
}

fn page_header(logical_pageid: i64) -> LogPageHeader {
    LogPageHeader {
        logical_pageid,
        offset: NULL_OFFSET,
        flags: 0,
        checksum: 0,
    }
}
