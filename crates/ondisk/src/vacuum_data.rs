//! The vacuum data file: its page layout, and emptying it on a target.
//!
//! Input: a target's [`SpaceManager`] and `BOOT_DB_PARM.vacuum_data_vfid`. Output: the file's first page holds no
//! entries and links no next page; every later page of its chain is deallocated.
//! Source: engine `src/query/vacuum.c` `struct vacuum_data_page` (`next_page` VPID @0, `index_unvacuumed` i16 @8,
//! `index_free` i16 @10, `data[]` @16), `struct vacuum_data_entry` (32 B: blockid @0, start_lsa @8,
//! `oldest_visible_mvccid` @16, `newest_mvccid` @24), `vacuum_data_initialize_new_page` (:4512-4524),
//! `vacuum_reset_data_after_copydb` (:7966-8001), `vacuum_data_mark_finished` (:4897-4966: a later page and the first
//! page are plain `file_dealloc` pages); develop @5b1867174, release/11.4 @46212adcd the same. R5 § 2.6, rule R4.
//! Offsets computed, read on T1 vacuum data pages (workspace#499) and on workspace#551 fixtures.

use std::io;

use zerocopy::little_endian::{I16, U64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use crate::Vfid;
use crate::file_alloc::SpaceManager;
use crate::file_header::{FileDescriptor, FileHeader};
use crate::ids::{Vpid, VpidLe};
use crate::page::{db_area, db_area_mut};
use crate::volume_set::VolumeSet;

/// Engine `VACUUM_NULL_LOG_BLOCKID`: the "last blockid" an empty first page holds after copydb.
const NULL_LOG_BLOCKID: u64 = u64::MAX;

/// Engine `struct vacuum_data_page` up to its first entry.
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
pub(crate) struct VacuumDataPageLe {
    pub next_page: VpidLe,
    pub index_unvacuumed: I16,
    pub index_free: I16,
    pad: [u8; 4],
    pub first: VacuumDataEntryLe,
}

/// Engine `struct vacuum_data_entry`.
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
pub(crate) struct VacuumDataEntryLe {
    pub blockid: U64,
    pub start_lsa: U64,
    pub oldest_visible_mvccid: U64,
    pub newest_mvccid: U64,
}

/// The first page of the vacuum data file `vfid` (`FILE_VACUUM_DATA_DES.vpid_first`).
pub(crate) fn first_page(vs: &VolumeSet, vfid: Vfid) -> io::Result<Vpid> {
    match FileHeader::read(&vs.read_page(vfid.header_vpid())?).descriptor() {
        FileDescriptor::VacuumData { vpid_first } => Ok(vpid_first),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{vfid:?} is not the vacuum data file"),
        )),
    }
}

/// Empties the vacuum data file `vfid` of a target. Returns the number of pages deallocated.
///
/// Its entries name blocks of the source log, which the target does not have (ADR 0004); the heap work they stand for
/// is done by the page-treatment phase, and every B-tree is rebuilt. The target's first boot runs
/// `vacuum_reset_data_after_copydb` (the new log is `was_copied`), which asserts an empty first page with no next page
/// (workspace#551: develop optdebug aborts on a source left with entries). The first page is initialized as
/// `vacuum_data_initialize_new_page` does, last blockid NULL as after copydb; it keeps its LSA (ADR 0010).
pub fn empty(space: &SpaceManager, vfid: Vfid) -> io::Result<usize> {
    let vs = space.volumes();
    let first = first_page(vs, vfid)?;
    let mut later = Vec::new();
    let mut page = vs.read_page(first)?;
    let mut next = header(&page).next_page.get();
    while !next.is_null() {
        if next == first || later.contains(&next) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("vacuum data chain of {vfid:?} loops at {next:?}"),
            ));
        }
        later.push(next);
        next = header(&vs.read_page(next)?).next_page.get();
    }
    let area = db_area_mut(&mut page);
    area.fill(0);
    let h = VacuumDataPageLe::mut_from_prefix(area)
        .expect("a page holds the vacuum data page header")
        .0;
    h.next_page.set(Vpid::NULL);
    h.first.blockid.set(NULL_LOG_BLOCKID);
    vs.write_page(first, &page)?;
    for vpid in &later {
        space.dealloc_page(vfid, *vpid)?;
    }
    Ok(later.len())
}

/// The page header of a vacuum data page.
pub(crate) fn header(page: &[u8]) -> &VacuumDataPageLe {
    VacuumDataPageLe::ref_from_prefix(db_area(page))
        .expect("a page holds the vacuum data page header")
        .0
}
