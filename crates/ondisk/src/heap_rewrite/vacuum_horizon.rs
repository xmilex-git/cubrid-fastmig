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
use zerocopy::little_endian::{I16, U64};
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

use crate::Vfid;
use crate::ids::VpidLe;
use crate::log_header::LogHeader;
use crate::page::db_area;
use crate::record::heap_chain::{HeapChain, VacuumStatus};
use crate::volume_set::VolumeSet;

/// Engine `struct vacuum_data_page` up to its first entry.
#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct VacuumDataPageLe {
    next_page: VpidLe,
    index_unvacuumed: I16,
    index_free: I16,
    pad: [u8; 4],
    first: VacuumDataEntryLe,
}

/// Engine `struct vacuum_data_entry`.
#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct VacuumDataEntryLe {
    blockid: U64,
    start_lsa: U64,
    oldest_visible_mvccid: U64,
    newest_mvccid: U64,
}

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
        let vdp = VacuumDataPageLe::ref_from_prefix(area)
            .expect("a page holds the vacuum data page header")
            .0;
        let empty = vdp.next_page.get().is_null() && vdp.index_unvacuumed == vdp.index_free;
        let oldest_visible = if log.does_block_need_vacuum {
            log.oldest_visible_mvccid
        } else {
            log.mvcc_next_id
        };
        Ok(VacuumHorizon {
            oldest_unvacuumed: if empty {
                oldest_visible
            } else {
                vdp.first.oldest_visible_mvccid.get()
            },
            pending: !empty || log.does_block_need_vacuum,
        })
    }

    /// Whether a data page's records must be cleaned: vacuum status ONCE or UNKNOWN with `max_mvccid` not below OU.
    /// NONE pages are vacuumed; so is a ONCE or UNKNOWN page whose every MVCC operation (all ≤ `max_mvccid`) is below
    /// OU (`vacuum_is_mvccid_vacuumed`) — a ONCE page keeps that status after its vacuum when the vacuum job had been
    /// interrupted by a shutdown and re-ran (engine `vacuum_heap_page`, develop @5b1867174 vacuum.c:1813-1816), so a
    /// source with no pending vacuum work can still hold ONCE pages (R2 § 3 R2.2; workspace#484, V1 cbrd_24871).
    pub fn page_needs_patch(&self, chain: &HeapChain) -> bool {
        chain.vacuum_status() != VacuumStatus::None && chain.max_mvccid >= self.oldest_unvacuumed
    }
}
