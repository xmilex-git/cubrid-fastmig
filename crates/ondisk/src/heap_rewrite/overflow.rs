//! A heap's overflow file: write a big record as an overflow page chain, list or release a chain, create the file
//! when the heap has none.
//!
//! Input: the [`SpaceManager`] of the target, the overflow file's VFID, a record or a first overflow page.
//! Output: the first page of a new chain, the pages of a chain, or a new `MultipageObjectHeap` file.
//! Source: engine `src/storage/overflow_file.c` `overflow_insert` (develop @5b1867174 :82-266; page count
//! `1 + ceil((len - (DB_PAGESIZE - 12)) / (DB_PAGESIZE - 8))`, `OVERFLOW_FIRST_PART` = next VPID @0, length i32 @8,
//! data @12; `OVERFLOW_REST_PART` = next VPID @0, data @8; pages of type PAGE_OVERFLOW), `heap_ovf_find_vfid`
//! (`heap_file.c` :6054-6149; `file_create_with_npages (FILE_MULTIPAGE_OBJECT_HEAP, 1, des)`,
//! `FILE_OVF_HEAP_DES` = HFID @0, class OID @12), `heap_ovf_delete` (`overflow_delete`: every page deallocated);
//! same in release/11.4 @46212adcd. R2 § 1.8.

use std::io;

use crate::file_alloc::{SpaceManager, perm_tablespace};
use crate::file_header::{FILE_DESCRIPTORS_SIZE, FileType};
use crate::overflow_part::{
    FIRST_DATA_OFFSET as FIRST_DATA, OverflowChain, OverflowFirstPart, OverflowRestPart,
    REST_DATA_OFFSET as REST_DATA,
};
use crate::page::{self, PageType, db_area, db_area_mut, db_pagesize};
use crate::{Hfid, Oid, Vfid, Vpid};

/// Writes `rec` as a new overflow chain of `ovf_vfid`.
pub fn write_chain(space: &SpaceManager, ovf_vfid: Vfid, rec: &[u8]) -> io::Result<OverflowChain> {
    let io_size = space.volumes().io_pagesize();
    let db = db_pagesize(io_size);
    let rest = rec.len().saturating_sub(db - FIRST_DATA);
    let npages = 1 + rest.div_ceil(db - REST_DATA);
    let vpids = (0..npages)
        .map(|_| space.alloc_page(ovf_vfid, PageType::Overflow))
        .collect::<io::Result<Vec<_>>>()?;
    let mut data = rec;
    for (i, &vpid) in vpids.iter().enumerate() {
        let mut p = vec![0u8; io_size];
        page::init_header(&mut p, vpid, PageType::Overflow);
        let area = db_area_mut(&mut p);
        let next = vpids.get(i + 1).copied().unwrap_or(Vpid::NULL);
        OverflowRestPart::of_mut(area).next_vpid.set(next);
        let at = if i == 0 {
            OverflowFirstPart::of_mut(area).length.set(rec.len() as i32);
            FIRST_DATA
        } else {
            REST_DATA
        };
        let n = data.len().min(area.len() - at);
        area[at..at + n].copy_from_slice(&data[..n]);
        data = &data[n..];
        space.volumes().write_page(vpid, &p)?;
    }
    Ok(OverflowChain {
        first: vpids[0],
        pages: npages,
    })
}

/// The pages of the chain starting at `first`, in chain order (bounded by the length the first page records).
pub fn chain_pages(space: &SpaceManager, first: Vpid) -> io::Result<Vec<Vpid>> {
    let vs = space.volumes();
    let db = db_pagesize(vs.io_pagesize());
    let p = vs.read_page(first)?;
    let len = OverflowFirstPart::of(db_area(&p)).length.get().max(0) as usize;
    let npages = 1 + len.saturating_sub(db - FIRST_DATA).div_ceil(db - REST_DATA);
    let mut out = vec![first];
    let mut next = OverflowRestPart::of(db_area(&p)).next_vpid.get();
    while !next.is_null() {
        if out.len() >= npages {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("overflow chain at {first:?} longer than its {len} B"),
            ));
        }
        out.push(next);
        next = OverflowRestPart::of(db_area(&vs.read_page(next)?))
            .next_vpid
            .get();
    }
    Ok(out)
}

/// Deallocates every page of the chain starting at `first`; returns how many.
pub fn release_chain(space: &SpaceManager, ovf_vfid: Vfid, first: Vpid) -> io::Result<usize> {
    let pages = chain_pages(space, first)?;
    for &v in &pages {
        space.dealloc_page(ovf_vfid, v)?;
    }
    Ok(pages.len())
}

/// Creates the overflow file of heap `hfid` (class `class_oid`), one page of initial size as the engine does.
pub fn create_file(space: &SpaceManager, hfid: Hfid, class_oid: Oid) -> io::Result<Vfid> {
    let mut des = [0u8; FILE_DESCRIPTORS_SIZE];
    hfid.write(&mut des, 0);
    class_oid.write(&mut des, 12);
    space.create_file(
        FileType::MultipageObjectHeap,
        des,
        perm_tablespace(space.volumes().io_pagesize(), 1),
    )
}

/// The home-slot body of a big record: the first overflow page as an OID with slot -1 (`heap_ovf_insert`).
pub fn home_pointer(first: Vpid) -> [u8; 8] {
    let mut b = [0u8; 8];
    Oid::new(first.volid, first.pageid, -1).write(&mut b, 0);
    b
}
