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

use std::mem::offset_of;

use zerocopy::little_endian::{F32, I16, I32, I64, U16, U64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use crate::log_page::LOG_PAGE_HEADER_SIZE;
use crate::lsa::{Lsa, LsaLe};
use crate::source_version::{HeaderLayout, SourceVersion};
use crate::value::{DecodeError, DecodeResult};

/// Engine magic `CUBRID_MAGIC_LOG_ACTIVE`.
pub const MAGIC: &str = "CUBRID/LogActive";
/// `sizeof (LOG_HEADER)`.
pub const LOG_HEADER_SIZE: usize = 384;
/// `sizeof (LOG_HEADER)` in 10.2..11.3.
pub const LOG_HEADER_SIZE_BEFORE_11_4: usize = 376;
/// Engine `REL_MAX_RELEASE_LENGTH`.
pub(crate) const DB_RELEASE_LEN: usize = 15;
/// Engine `MAXLOGNAME`.
const PREFIX_NAME_LEN: usize = 18;
/// `bkinfo[FILEIO_BACKUP_UNDEFINED_LEVEL]`, 3 × 32 B, kept as raw bytes.
const BKINFO_LEN: usize = 96;

/// Engine `LOG_HEADER` as stored, after the log page header (add [`LOG_PAGE_HEADER_SIZE`] for the file offset).
/// Bytes 102..104 are `sysmeta_version` in develop and `dummy3`/`dummy4` (0) in 11.4; nothing else differs.
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
pub struct LogHeaderLe {
    /// @0, `CUBRID_MAGIC_MAX_LENGTH` = 25.
    pub magic: [u8; 25],
    pad0: [u8; 3],
    /// @28
    pub cdc_arv_num_to_keep: I32,
    /// @32
    pub db_creation: I64,
    /// @40
    pub vol_creation: I64,
    /// @48
    pub db_release: [u8; DB_RELEASE_LEN],
    pad1: u8,
    /// @64
    pub db_compatibility: F32,
    /// @68
    pub db_iopagesize: I16,
    /// @70
    pub db_logpagesize: I16,
    /// @72
    pub is_shutdown: u8,
    pad2: [u8; 3],
    /// @76
    pub next_trid: I32,
    /// @80
    pub mvcc_next_id: U64,
    /// @88
    pub avg_ntrans: I32,
    /// @92
    pub avg_nlocks: I32,
    /// @96
    pub npages: I32,
    /// @100
    pub db_charset: i8,
    /// @101
    pub was_copied: u8,
    /// @102
    pub sysmeta_version: U16,
    /// @104
    pub fpageid: I64,
    /// @112
    pub append_lsa: LsaLe,
    /// @120
    pub chkpt_lsa: LsaLe,
    /// @128
    pub nxarv_pageid: I64,
    /// @136
    pub nxarv_phy_pageid: I32,
    /// @140
    pub nxarv_num: I32,
    /// @144
    pub last_arv_num_for_syscrashes: I32,
    /// @148
    pub last_deleted_arv_num: I32,
    /// @152, levels 0..2.
    pub bkup_level_lsa: [LsaLe; 3],
    /// @176
    pub prefix_name: [u8; PREFIX_NAME_LEN],
    /// @194
    pub has_logging_been_skipped: u8,
    pad3: [u8; 5],
    /// @200
    pub vacuum_last_blockid: I64,
    /// @208
    pub perm_status_obsolete: I32,
    pad4: [u8; 4],
    /// @216
    pub bkinfo: [u8; BKINFO_LEN],
    /// @312
    pub ha_server_state: I32,
    /// @316
    pub ha_file_status: I32,
    /// @320
    pub eof_lsa: LsaLe,
    /// @328
    pub smallest_lsa_at_last_chkpt: LsaLe,
    /// @336
    pub mvcc_op_log_lsa: LsaLe,
    /// @344
    pub oldest_visible_mvccid: U64,
    /// @352
    pub newest_block_mvccid: U64,
    /// @360
    pub ha_promotion_time: I64,
    /// @368
    pub db_restore_time: I64,
    /// @376
    pub mark_will_del: u8,
    /// @377
    pub does_block_need_vacuum: u8,
    /// @378
    pub was_active_log_reset: u8,
    pad5: [u8; 5],
}
const _: () = assert!(size_of::<LogHeaderLe>() == LOG_HEADER_SIZE);

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
    let magic_at = LOG_PAGE_HEADER_SIZE + offset_of!(LogHeaderLe, magic);
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
            let gap = LOG_PAGE_HEADER_SIZE + offset_of!(LogHeaderLe, vol_creation);
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
        let h = LogHeaderLe::ref_from_prefix(&page[LOG_PAGE_HEADER_SIZE..])
            .expect("a log page holds the header")
            .0;
        LogHeader {
            magic: cstr(&h.magic),
            cdc_arv_num_to_keep: h.cdc_arv_num_to_keep.get(),
            db_creation: h.db_creation.get(),
            vol_creation: Some(h.vol_creation.get()),
            db_release: cstr(&h.db_release),
            db_compatibility: h.db_compatibility.get(),
            db_iopagesize: h.db_iopagesize.get(),
            db_logpagesize: h.db_logpagesize.get(),
            is_shutdown: h.is_shutdown != 0,
            next_trid: h.next_trid.get(),
            mvcc_next_id: h.mvcc_next_id.get(),
            avg_ntrans: h.avg_ntrans.get(),
            avg_nlocks: h.avg_nlocks.get(),
            npages: h.npages.get(),
            db_charset: h.db_charset,
            was_copied: h.was_copied != 0,
            sysmeta_version: h.sysmeta_version.get(),
            fpageid: h.fpageid.get(),
            append_lsa: h.append_lsa.get(),
            chkpt_lsa: h.chkpt_lsa.get(),
            nxarv_pageid: h.nxarv_pageid.get(),
            nxarv_phy_pageid: h.nxarv_phy_pageid.get(),
            nxarv_num: h.nxarv_num.get(),
            last_arv_num_for_syscrashes: h.last_arv_num_for_syscrashes.get(),
            last_deleted_arv_num: h.last_deleted_arv_num.get(),
            bkup_level_lsa: h.bkup_level_lsa.map(|l| l.get()),
            prefix_name: cstr(&h.prefix_name),
            has_logging_been_skipped: h.has_logging_been_skipped != 0,
            vacuum_last_blockid: h.vacuum_last_blockid.get(),
            perm_status_obsolete: h.perm_status_obsolete.get(),
            bkinfo: h.bkinfo,
            ha_server_state: h.ha_server_state.get(),
            ha_file_status: h.ha_file_status.get(),
            eof_lsa: h.eof_lsa.get(),
            smallest_lsa_at_last_chkpt: h.smallest_lsa_at_last_chkpt.get(),
            mvcc_op_log_lsa: h.mvcc_op_log_lsa.get(),
            oldest_visible_mvccid: h.oldest_visible_mvccid.get(),
            newest_block_mvccid: h.newest_block_mvccid.get(),
            ha_promotion_time: h.ha_promotion_time.get(),
            db_restore_time: h.db_restore_time.get(),
            mark_will_del: h.mark_will_del != 0,
            does_block_need_vacuum: h.does_block_need_vacuum != 0,
            was_active_log_reset: h.was_active_log_reset != 0,
        }
    }

    /// Encodes every field into the header area of `page` (bytes 16..400), padding zeroed. The log page header and
    /// checksum are left to [`crate::log_page`].
    pub fn write(&self, page: &mut [u8]) {
        let h = LogHeaderLe::mut_from_prefix(&mut page[LOG_PAGE_HEADER_SIZE..])
            .expect("a log page holds the header")
            .0;
        h.as_mut_bytes().fill(0);
        put_cstr(&mut h.magic, &self.magic);
        h.cdc_arv_num_to_keep.set(self.cdc_arv_num_to_keep);
        h.db_creation.set(self.db_creation);
        h.vol_creation.set(
            self.vol_creation
                .expect("the tool writes the 384 B layout only"),
        );
        put_cstr(&mut h.db_release, &self.db_release);
        h.db_compatibility.set(self.db_compatibility);
        h.db_iopagesize.set(self.db_iopagesize);
        h.db_logpagesize.set(self.db_logpagesize);
        h.is_shutdown = self.is_shutdown as u8;
        h.next_trid.set(self.next_trid);
        h.mvcc_next_id.set(self.mvcc_next_id);
        h.avg_ntrans.set(self.avg_ntrans);
        h.avg_nlocks.set(self.avg_nlocks);
        h.npages.set(self.npages);
        h.db_charset = self.db_charset;
        h.was_copied = self.was_copied as u8;
        h.sysmeta_version.set(self.sysmeta_version);
        h.fpageid.set(self.fpageid);
        h.append_lsa.set(self.append_lsa);
        h.chkpt_lsa.set(self.chkpt_lsa);
        h.nxarv_pageid.set(self.nxarv_pageid);
        h.nxarv_phy_pageid.set(self.nxarv_phy_pageid);
        h.nxarv_num.set(self.nxarv_num);
        h.last_arv_num_for_syscrashes
            .set(self.last_arv_num_for_syscrashes);
        h.last_deleted_arv_num.set(self.last_deleted_arv_num);
        for (le, lsa) in h.bkup_level_lsa.iter_mut().zip(self.bkup_level_lsa) {
            le.set(lsa);
        }
        put_cstr(&mut h.prefix_name, &self.prefix_name);
        h.has_logging_been_skipped = self.has_logging_been_skipped as u8;
        h.vacuum_last_blockid.set(self.vacuum_last_blockid);
        h.perm_status_obsolete.set(self.perm_status_obsolete);
        h.bkinfo = self.bkinfo;
        h.ha_server_state.set(self.ha_server_state);
        h.ha_file_status.set(self.ha_file_status);
        h.eof_lsa.set(self.eof_lsa);
        h.smallest_lsa_at_last_chkpt
            .set(self.smallest_lsa_at_last_chkpt);
        h.mvcc_op_log_lsa.set(self.mvcc_op_log_lsa);
        h.oldest_visible_mvccid.set(self.oldest_visible_mvccid);
        h.newest_block_mvccid.set(self.newest_block_mvccid);
        h.ha_promotion_time.set(self.ha_promotion_time);
        h.db_restore_time.set(self.db_restore_time);
        h.mark_will_del = self.mark_will_del as u8;
        h.does_block_need_vacuum = self.does_block_need_vacuum as u8;
        h.was_active_log_reset = self.was_active_log_reset as u8;
    }
}
