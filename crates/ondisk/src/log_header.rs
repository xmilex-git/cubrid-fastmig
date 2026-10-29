//! The active log header (`LOG_HEADER`, physical page 0 of `<db>_lgat` after the log page header): read and write.
//!
//! Input: the header page of an active log (`db_logpagesize` bytes). Output: [`LogHeader`], or the page area
//! rewritten from one. The page's own header and checksum are [`crate::log_page`]'s.
//! Source: engine `src/transaction/log_storage.hpp:113-169` (develop @5b1867174; release/11.4 @46212adcd the same
//! except bytes 102..104, `dummy3`/`dummy4` in 11.4 and `sysmeta_version` in develop, CUBRID/cubrid#7773);
//! R5 § 2.7. Size 384 B in both. Offsets checked on the T1 fm114_clean `_lgat` (append_lsa 9205|304, workspace#490).
//! 10.2..11.3 write the 376 B layout: no `vol_creation`, every later field 8 B lower, `dummy` at +28 and
//! `dummy3`/`dummy4` at +94 (R8 § 2.14, gdb `ptype /o` per install; checked on a createdb `_lgat` of 10.2.18,
//! 11.0.16, 11.2.9 and 11.3.6, workspace#522). [`read_source`] decodes either layout.

use crate::le_field::{
    get_f32, get_i16, get_i32, get_i64, get_u16, put_f32, put_i16, put_i32, put_i64, put_u16,
};
use crate::log_page::LOG_PAGE_HEADER_SIZE;
use crate::lsa::Lsa;
use crate::source_version::{HeaderLayout, SourceVersion};
use crate::value::{DecodeError, DecodeResult};

/// Engine magic `CUBRID_MAGIC_LOG_ACTIVE`.
pub const MAGIC: &str = "CUBRID/LogActive";
/// `sizeof (LOG_HEADER)`.
pub const LOG_HEADER_SIZE: usize = 384;
/// `sizeof (LOG_HEADER)` in 10.2..11.3.
pub const LOG_HEADER_SIZE_BEFORE_11_4: usize = 376;
/// Engine `REL_MAX_RELEASE_LENGTH`.
const DB_RELEASE_LEN: usize = 15;
/// Engine `MAXLOGNAME`.
const PREFIX_NAME_LEN: usize = 18;
/// `bkinfo[FILEIO_BACKUP_UNDEFINED_LEVEL]`, 3 × 32 B, kept as raw bytes.
const BKINFO_LEN: usize = 96;

/// Field offsets inside `LOG_HEADER` (add [`LOG_PAGE_HEADER_SIZE`] for the file offset).
pub mod off {
    pub const MAGIC: usize = 0;
    pub const CDC_ARV_NUM_TO_KEEP: usize = 28;
    pub const DB_CREATION: usize = 32;
    pub const VOL_CREATION: usize = 40;
    pub const DB_RELEASE: usize = 48;
    pub const DB_COMPATIBILITY: usize = 64;
    pub const DB_IOPAGESIZE: usize = 68;
    pub const DB_LOGPAGESIZE: usize = 70;
    pub const IS_SHUTDOWN: usize = 72;
    pub const NEXT_TRID: usize = 76;
    pub const MVCC_NEXT_ID: usize = 80;
    pub const AVG_NTRANS: usize = 88;
    pub const AVG_NLOCKS: usize = 92;
    pub const NPAGES: usize = 96;
    pub const DB_CHARSET: usize = 100;
    pub const WAS_COPIED: usize = 101;
    pub const SYSMETA_VERSION: usize = 102;
    pub const FPAGEID: usize = 104;
    pub const APPEND_LSA: usize = 112;
    pub const CHKPT_LSA: usize = 120;
    pub const NXARV_PAGEID: usize = 128;
    pub const NXARV_PHY_PAGEID: usize = 136;
    pub const NXARV_NUM: usize = 140;
    pub const LAST_ARV_NUM_FOR_SYSCRASHES: usize = 144;
    pub const LAST_DELETED_ARV_NUM: usize = 148;
    pub const BKUP_LEVEL0_LSA: usize = 152;
    pub const BKUP_LEVEL1_LSA: usize = 160;
    pub const BKUP_LEVEL2_LSA: usize = 168;
    pub const PREFIX_NAME: usize = 176;
    pub const HAS_LOGGING_BEEN_SKIPPED: usize = 194;
    pub const VACUUM_LAST_BLOCKID: usize = 200;
    pub const PERM_STATUS_OBSOLETE: usize = 208;
    pub const BKINFO: usize = 216;
    pub const HA_SERVER_STATE: usize = 312;
    pub const HA_FILE_STATUS: usize = 316;
    pub const EOF_LSA: usize = 320;
    pub const SMALLEST_LSA_AT_LAST_CHKPT: usize = 328;
    pub const MVCC_OP_LOG_LSA: usize = 336;
    pub const OLDEST_VISIBLE_MVCCID: usize = 344;
    pub const NEWEST_BLOCK_MVCCID: usize = 352;
    pub const HA_PROMOTION_TIME: usize = 360;
    pub const DB_RESTORE_TIME: usize = 368;
    pub const MARK_WILL_DEL: usize = 376;
    pub const DOES_BLOCK_NEED_VACUUM: usize = 377;
    pub const WAS_ACTIVE_LOG_RESET: usize = 378;
}

/// Engine `LOG_HEADER`, every field. Padding is not kept; [`LogHeader::write`] zeroes it.
#[derive(Clone, Debug, PartialEq)]
pub struct LogHeader {
    pub magic: String,
    pub cdc_arv_num_to_keep: i32,
    pub db_creation: i64,
    /// `None` in a 10.2..11.3 header (the field does not exist).
    pub vol_creation: Option<i64>,
    pub db_release: String,
    pub db_compatibility: f32,
    pub db_iopagesize: i16,
    pub db_logpagesize: i16,
    pub is_shutdown: bool,
    pub next_trid: i32,
    pub mvcc_next_id: u64,
    pub avg_ntrans: i32,
    pub avg_nlocks: i32,
    /// Pages in the active portion, header page excluded.
    pub npages: i32,
    pub db_charset: i8,
    pub was_copied: bool,
    /// develop `sysmeta_version`; 11.4 `dummy3`/`dummy4` (0).
    pub sysmeta_version: u16,
    /// Logical page id at physical page 1.
    pub fpageid: i64,
    pub append_lsa: Lsa,
    pub chkpt_lsa: Lsa,
    pub nxarv_pageid: i64,
    pub nxarv_phy_pageid: i32,
    pub nxarv_num: i32,
    pub last_arv_num_for_syscrashes: i32,
    pub last_deleted_arv_num: i32,
    pub bkup_level_lsa: [Lsa; 3],
    pub prefix_name: String,
    pub has_logging_been_skipped: bool,
    pub vacuum_last_blockid: i64,
    pub perm_status_obsolete: i32,
    pub bkinfo: [u8; BKINFO_LEN],
    pub ha_server_state: i32,
    pub ha_file_status: i32,
    pub eof_lsa: Lsa,
    pub smallest_lsa_at_last_chkpt: Lsa,
    pub mvcc_op_log_lsa: Lsa,
    pub oldest_visible_mvccid: u64,
    pub newest_block_mvccid: u64,
    pub ha_promotion_time: i64,
    pub db_restore_time: i64,
    pub mark_will_del: bool,
    pub does_block_need_vacuum: bool,
    pub was_active_log_reset: bool,
}

fn cstr(buf: &[u8]) -> String {
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

fn put_cstr(buf: &mut [u8], s: &str) {
    buf.fill(0);
    let n = s.len().min(buf.len() - 1);
    buf[..n].copy_from_slice(&s.as_bytes()[..n]);
}

/// Decodes a source's header in the layout its release wrote, with the [`SourceVersion`] the header names. Refuses a
/// page shorter than its layout or whose magic is not [`MAGIC`].
pub fn read_source(page: &[u8]) -> DecodeResult<(SourceVersion, LogHeader)> {
    let magic_at = LOG_PAGE_HEADER_SIZE + off::MAGIC;
    if page.get(magic_at..magic_at + MAGIC.len()) != Some(MAGIC.as_bytes()) {
        return Err(DecodeError {
            at: magic_at,
            what: "not an active log (magic)".into(),
        });
    }
    let version = SourceVersion::detect(page)?;
    let size = match version.header_layout() {
        HeaderLayout::Since11_4 => LOG_HEADER_SIZE,
        HeaderLayout::Before11_4 => LOG_HEADER_SIZE_BEFORE_11_4,
    };
    let Some(area) = page.get(..LOG_PAGE_HEADER_SIZE + size) else {
        return Err(DecodeError {
            at: page.len(),
            what: format!("log header page shorter than {size} B"),
        });
    };
    let header = match version.header_layout() {
        HeaderLayout::Since11_4 => LogHeader::read(area),
        HeaderLayout::Before11_4 => {
            // the 376 B layout is the 384 B one without vol_creation: open its 8 B again and decode the result
            let gap = LOG_PAGE_HEADER_SIZE + off::VOL_CREATION;
            let mut wide = vec![0u8; LOG_PAGE_HEADER_SIZE + LOG_HEADER_SIZE];
            wide[..gap].copy_from_slice(&area[..gap]);
            wide[gap + 8..].copy_from_slice(&area[gap..]);
            LogHeader {
                vol_creation: None,
                ..LogHeader::read(&wide)
            }
        }
    };
    Ok((version, header))
}

impl LogHeader {
    /// Decodes the 384 B header (11.4, develop) from the active log's header page (whole log page).
    pub fn read(page: &[u8]) -> Self {
        let h = &page[LOG_PAGE_HEADER_SIZE..LOG_PAGE_HEADER_SIZE + LOG_HEADER_SIZE];
        let mut bkinfo = [0u8; BKINFO_LEN];
        bkinfo.copy_from_slice(&h[off::BKINFO..off::BKINFO + BKINFO_LEN]);
        LogHeader {
            magic: cstr(&h[off::MAGIC..off::MAGIC + 25]),
            cdc_arv_num_to_keep: get_i32(h, off::CDC_ARV_NUM_TO_KEEP),
            db_creation: get_i64(h, off::DB_CREATION),
            vol_creation: Some(get_i64(h, off::VOL_CREATION)),
            db_release: cstr(&h[off::DB_RELEASE..off::DB_RELEASE + DB_RELEASE_LEN]),
            db_compatibility: get_f32(h, off::DB_COMPATIBILITY),
            db_iopagesize: get_i16(h, off::DB_IOPAGESIZE),
            db_logpagesize: get_i16(h, off::DB_LOGPAGESIZE),
            is_shutdown: h[off::IS_SHUTDOWN] != 0,
            next_trid: get_i32(h, off::NEXT_TRID),
            mvcc_next_id: get_i64(h, off::MVCC_NEXT_ID) as u64,
            avg_ntrans: get_i32(h, off::AVG_NTRANS),
            avg_nlocks: get_i32(h, off::AVG_NLOCKS),
            npages: get_i32(h, off::NPAGES),
            db_charset: h[off::DB_CHARSET] as i8,
            was_copied: h[off::WAS_COPIED] != 0,
            sysmeta_version: get_u16(h, off::SYSMETA_VERSION),
            fpageid: get_i64(h, off::FPAGEID),
            append_lsa: Lsa::read(h, off::APPEND_LSA),
            chkpt_lsa: Lsa::read(h, off::CHKPT_LSA),
            nxarv_pageid: get_i64(h, off::NXARV_PAGEID),
            nxarv_phy_pageid: get_i32(h, off::NXARV_PHY_PAGEID),
            nxarv_num: get_i32(h, off::NXARV_NUM),
            last_arv_num_for_syscrashes: get_i32(h, off::LAST_ARV_NUM_FOR_SYSCRASHES),
            last_deleted_arv_num: get_i32(h, off::LAST_DELETED_ARV_NUM),
            bkup_level_lsa: [
                Lsa::read(h, off::BKUP_LEVEL0_LSA),
                Lsa::read(h, off::BKUP_LEVEL1_LSA),
                Lsa::read(h, off::BKUP_LEVEL2_LSA),
            ],
            prefix_name: cstr(&h[off::PREFIX_NAME..off::PREFIX_NAME + PREFIX_NAME_LEN]),
            has_logging_been_skipped: h[off::HAS_LOGGING_BEEN_SKIPPED] != 0,
            vacuum_last_blockid: get_i64(h, off::VACUUM_LAST_BLOCKID),
            perm_status_obsolete: get_i32(h, off::PERM_STATUS_OBSOLETE),
            bkinfo,
            ha_server_state: get_i32(h, off::HA_SERVER_STATE),
            ha_file_status: get_i32(h, off::HA_FILE_STATUS),
            eof_lsa: Lsa::read(h, off::EOF_LSA),
            smallest_lsa_at_last_chkpt: Lsa::read(h, off::SMALLEST_LSA_AT_LAST_CHKPT),
            mvcc_op_log_lsa: Lsa::read(h, off::MVCC_OP_LOG_LSA),
            oldest_visible_mvccid: get_i64(h, off::OLDEST_VISIBLE_MVCCID) as u64,
            newest_block_mvccid: get_i64(h, off::NEWEST_BLOCK_MVCCID) as u64,
            ha_promotion_time: get_i64(h, off::HA_PROMOTION_TIME),
            db_restore_time: get_i64(h, off::DB_RESTORE_TIME),
            mark_will_del: h[off::MARK_WILL_DEL] != 0,
            does_block_need_vacuum: h[off::DOES_BLOCK_NEED_VACUUM] != 0,
            was_active_log_reset: h[off::WAS_ACTIVE_LOG_RESET] != 0,
        }
    }

    /// Encodes every field into the header area of `page` (bytes 16..400), padding zeroed. The log page header and
    /// checksum are left to [`crate::log_page`].
    pub fn write(&self, page: &mut [u8]) {
        let h = &mut page[LOG_PAGE_HEADER_SIZE..LOG_PAGE_HEADER_SIZE + LOG_HEADER_SIZE];
        h.fill(0);
        put_cstr(&mut h[off::MAGIC..off::MAGIC + 25], &self.magic);
        put_i32(h, off::CDC_ARV_NUM_TO_KEEP, self.cdc_arv_num_to_keep);
        put_i64(h, off::DB_CREATION, self.db_creation);
        let vol_creation = self
            .vol_creation
            .expect("the tool writes the 384 B layout only");
        put_i64(h, off::VOL_CREATION, vol_creation);
        put_cstr(
            &mut h[off::DB_RELEASE..off::DB_RELEASE + DB_RELEASE_LEN],
            &self.db_release,
        );
        put_f32(h, off::DB_COMPATIBILITY, self.db_compatibility);
        put_i16(h, off::DB_IOPAGESIZE, self.db_iopagesize);
        put_i16(h, off::DB_LOGPAGESIZE, self.db_logpagesize);
        h[off::IS_SHUTDOWN] = self.is_shutdown as u8;
        put_i32(h, off::NEXT_TRID, self.next_trid);
        put_i64(h, off::MVCC_NEXT_ID, self.mvcc_next_id as i64);
        put_i32(h, off::AVG_NTRANS, self.avg_ntrans);
        put_i32(h, off::AVG_NLOCKS, self.avg_nlocks);
        put_i32(h, off::NPAGES, self.npages);
        h[off::DB_CHARSET] = self.db_charset as u8;
        h[off::WAS_COPIED] = self.was_copied as u8;
        put_u16(h, off::SYSMETA_VERSION, self.sysmeta_version);
        put_i64(h, off::FPAGEID, self.fpageid);
        self.append_lsa.write(h, off::APPEND_LSA);
        self.chkpt_lsa.write(h, off::CHKPT_LSA);
        put_i64(h, off::NXARV_PAGEID, self.nxarv_pageid);
        put_i32(h, off::NXARV_PHY_PAGEID, self.nxarv_phy_pageid);
        put_i32(h, off::NXARV_NUM, self.nxarv_num);
        put_i32(
            h,
            off::LAST_ARV_NUM_FOR_SYSCRASHES,
            self.last_arv_num_for_syscrashes,
        );
        put_i32(h, off::LAST_DELETED_ARV_NUM, self.last_deleted_arv_num);
        for (i, o) in [
            off::BKUP_LEVEL0_LSA,
            off::BKUP_LEVEL1_LSA,
            off::BKUP_LEVEL2_LSA,
        ]
        .into_iter()
        .enumerate()
        {
            self.bkup_level_lsa[i].write(h, o);
        }
        put_cstr(
            &mut h[off::PREFIX_NAME..off::PREFIX_NAME + PREFIX_NAME_LEN],
            &self.prefix_name,
        );
        h[off::HAS_LOGGING_BEEN_SKIPPED] = self.has_logging_been_skipped as u8;
        put_i64(h, off::VACUUM_LAST_BLOCKID, self.vacuum_last_blockid);
        put_i32(h, off::PERM_STATUS_OBSOLETE, self.perm_status_obsolete);
        h[off::BKINFO..off::BKINFO + BKINFO_LEN].copy_from_slice(&self.bkinfo);
        put_i32(h, off::HA_SERVER_STATE, self.ha_server_state);
        put_i32(h, off::HA_FILE_STATUS, self.ha_file_status);
        self.eof_lsa.write(h, off::EOF_LSA);
        self.smallest_lsa_at_last_chkpt
            .write(h, off::SMALLEST_LSA_AT_LAST_CHKPT);
        self.mvcc_op_log_lsa.write(h, off::MVCC_OP_LOG_LSA);
        put_i64(
            h,
            off::OLDEST_VISIBLE_MVCCID,
            self.oldest_visible_mvccid as i64,
        );
        put_i64(h, off::NEWEST_BLOCK_MVCCID, self.newest_block_mvccid as i64);
        put_i64(h, off::HA_PROMOTION_TIME, self.ha_promotion_time);
        put_i64(h, off::DB_RESTORE_TIME, self.db_restore_time);
        h[off::MARK_WILL_DEL] = self.mark_will_del as u8;
        h[off::DOES_BLOCK_NEED_VACUUM] = self.does_block_need_vacuum as u8;
        h[off::WAS_ACTIVE_LOG_RESET] = self.was_active_log_reset as u8;
    }
}
