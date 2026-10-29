//! The source's vacuum horizon: the oldest unvacuumed MVCCID (OU), whether any vacuum work was pending at shutdown,
//! and which heap pages therefore still carry vacuum residue.
//!
//! Input: the source log header and its vacuum data file. Output: [`VacuumHorizon`]; [`VacuumHorizon::page_needs_patch`]
//! for one page's chain record.
//! Source: engine `src/query/vacuum.c` `vacuum_data::set_oldest_unvacuumed_on_boot` (develop @5b1867174 :8104-8122,
//! release/11.4 @46212adcd :8027-8045, same), `vacuum_data::is_empty` / `vacuum_data_page::is_empty`,
//! `struct vacuum_data_page` (`next_page` VPID @0, `index_unvacuumed` i16 @8, `index_free` i16 @10, `data[]` @16)
//! and `struct vacuum_data_entry` (32 B: blockid @0, start_lsa @8, `oldest_visible_mvccid` @16, `newest_mvccid`
//! @24), `vacuum_is_mvccid_vacuumed` (:7478-7486); R2 § 3 R2 (page selection). Offsets computed, read on T1
//! vacuum data pages (workspace#499).

use std::io;

use crate::file_header::{FileDescriptor, FileHeader};
use crate::le_field::{get_i16, get_u64};
use crate::log_header::LogHeader;
use crate::page::db_area;
use crate::record::heap_chain::{FLAG_VACUUM_STATUS_MASK, HeapChain};
use crate::volume_set::VolumeSet;
use crate::{Vfid, Vpid};

/// `HEAP_PAGE_FLAG_VACUUM_ONCE`.
const VACUUM_ONCE: u32 = 0x8000_0000;
/// Offset of `data[0].oldest_visible_mvccid` in a vacuum data page.
const FIRST_ENTRY_OLDEST_VISIBLE: usize = 16 + 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VacuumHorizon {
    /// Oldest unvacuumed MVCCID at the source's next boot.
    pub oldest_unvacuumed: u64,
    /// Vacuum data held blocks or the last log block still needed vacuum. When false no data page can be a
    /// patch target (R2 § 3 R2.4), so heaps without other work need not be walked.
    pub pending: bool,
}

impl VacuumHorizon {
    /// Reads the horizon of the source whose log header is `log` and vacuum data file `vacuum_data_vfid`
    /// (`BOOT_DB_PARM.vacuum_data_vfid`).
    pub fn read(
        vs: &VolumeSet,
        log: &LogHeader,
        vacuum_data_vfid: Vfid,
    ) -> io::Result<VacuumHorizon> {
        let header = FileHeader::read(&vs.read_page(vacuum_data_vfid.header_vpid())?);
        let FileDescriptor::VacuumData { vpid_first } = header.descriptor() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{vacuum_data_vfid:?} is not the vacuum data file"),
            ));
        };
        let page = vs.read_page(vpid_first)?;
        let area = db_area(&page);
        let empty = Vpid::read(area, 0).is_null() && get_i16(area, 8) == get_i16(area, 10);
        let oldest_visible = if log.does_block_need_vacuum {
            log.oldest_visible_mvccid
        } else {
            log.mvcc_next_id
        };
        Ok(VacuumHorizon {
            oldest_unvacuumed: if empty {
                oldest_visible
            } else {
                get_u64(area, FIRST_ENTRY_OLDEST_VISIBLE)
            },
            pending: !empty || log.does_block_need_vacuum,
        })
    }

    /// Whether a data page's records must be cleaned: vacuum status ONCE, or UNKNOWN with `max_mvccid` not below
    /// OU. NONE pages and older UNKNOWN pages are vacuumed as far as the engine is concerned (R2 § 3 R2.2).
    pub fn page_needs_patch(&self, chain: &HeapChain) -> bool {
        match chain.flags & FLAG_VACUUM_STATUS_MASK {
            0 => false,
            VACUUM_ONCE => true,
            _ => chain.max_mvccid >= self.oldest_unvacuumed,
        }
    }
}
