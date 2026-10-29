//! Iterates the records of one heap file: every object's OID and its whole record bytes.
//!
//! Input: an open [`VolumeSet`] and the heap's HFID. Output: [`HeapRecord`]s in page-chain and slot order, each the
//! complete record — a REC_RELOCATION home is resolved to its REC_NEWHOME bytes, a REC_BIGONE home to the bytes
//! assembled from its overflow pages. Deleted and address-only slots are skipped; MVCC visibility is the caller's
//! (see [`crate::record::mvcc_header`]). Memory is one page plus the record being returned, whatever the heap's size.
//! Source: engine `src/storage/heap_file.c` `heap_next_internal`/`heap_get_record_data_when_all_ready` (record
//! types), `heap_page_next` (skips `HEAP_PAGE_IS_BESTSPACE`, develop :2816-2832), `heap_ovf_get`;
//! `src/storage/overflow_file.h` `OVERFLOW_FIRST_PART`/`OVERFLOW_REST_PART` (native, same in 11.4 and develop);
//! R2 § 1.3, § 1.8.

use std::io;

use crate::le_field::get_i32;
use crate::page::db_area;
use crate::record::heap_chain::{HeapChain, header_next_vpid};
use crate::spage::{RecordType, SlottedPage};
use crate::volume_set::VolumeSet;
use crate::{Hfid, Oid, Vpid};

/// How a record's home slot holds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HomeKind {
    /// REC_HOME: the record is in its home slot.
    Home,
    /// REC_RELOCATION: the home slot holds the OID of a REC_NEWHOME slot.
    Relocated { newhome: Oid },
    /// REC_BIGONE: the home slot holds the VPID of the first overflow page.
    Big { first_overflow: Vpid },
}

/// One object of the heap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeapRecord {
    /// The home slot: the object's OID.
    pub oid: Oid,
    pub kind: HomeKind,
    /// The whole record (MVCC header + body).
    pub bytes: Vec<u8>,
}

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

fn slotted(page: &[u8], vpid: Vpid) -> io::Result<SlottedPage<'_>> {
    SlottedPage::new(db_area(page))
        .ok_or_else(|| invalid(format!("heap page {vpid:?} is not a slotted page")))
}

/// Overflow page layouts (engine `OVERFLOW_FIRST_PART`: `next_vpid` @0, `length` i32 @8, data @12;
/// `OVERFLOW_REST_PART`: `next_vpid` @0, data @8), in the DB area of each overflow page.
pub(crate) fn read_overflow(vs: &VolumeSet, first: Vpid) -> io::Result<Vec<u8>> {
    let page = vs.read_page(first)?;
    let area = db_area(&page);
    let length = get_i32(area, 8);
    if length < 0 {
        return Err(invalid(format!("overflow {first:?}: length {length}")));
    }
    let length = length as usize;
    let mut out = Vec::with_capacity(length);
    let take = (area.len() - 12).min(length);
    out.extend_from_slice(&area[12..12 + take]);
    let mut next = Vpid::read(area, 0);
    // a chain longer than the bytes it carries is a cycle; counting keeps memory constant
    let max_pages = length / (area.len() - 8) + 1;
    let mut pages = 1;
    while out.len() < length {
        pages += 1;
        if next.is_null() || pages > max_pages {
            return Err(invalid(format!(
                "overflow {first:?}: chain ends at {} of {length} bytes",
                out.len()
            )));
        }
        let page = vs.read_page(next)?;
        let area = db_area(&page);
        let take = (area.len() - 8).min(length - out.len());
        out.extend_from_slice(&area[8..8 + take]);
        next = Vpid::read(area, 0);
    }
    Ok(out)
}

/// Iterator over a heap file's records. Yields an error and stops on a broken chain or a dangling home slot.
pub struct HeapScan<'a> {
    vs: &'a VolumeSet,
    next_page: Vpid,
    page: Option<(Vpid, Box<[u8]>)>,
    slot: usize,
    /// Pages walked so far, and the most a chain can have (every page of the volume set): past it the chain loops.
    walked: u64,
    max_pages: u64,
    failed: bool,
}

/// Starts a scan of the heap whose header page is `hfid.vpid()`; the header page's own records come first.
pub fn heap_records(vs: &VolumeSet, hfid: Hfid) -> HeapScan<'_> {
    HeapScan {
        vs,
        next_page: hfid.vpid(),
        page: None,
        slot: 1,
        walked: 0,
        max_pages: vs
            .volumes()
            .iter()
            .map(|v| v.header.nsect_total.max(0) as u64 * v.header.sect_npgs.max(0) as u64)
            .sum(),
        failed: false,
    }
}

impl HeapScan<'_> {
    /// Loads the next record page of the chain (skipping develop bestspace pages); `None` at the chain's end.
    fn load_next_page(&mut self) -> io::Result<bool> {
        loop {
            let vpid = self.next_page;
            if vpid.is_null() {
                return Ok(false);
            }
            self.walked += 1;
            if self.walked > self.max_pages {
                return Err(invalid(format!(
                    "heap chain longer than the volume set at {vpid:?}"
                )));
            }
            let page = self.vs.read_page(vpid)?;
            let sp = slotted(&page, vpid)?;
            let (_, slot0) = sp
                .record(0)
                .ok_or_else(|| invalid(format!("heap page {vpid:?} has no slot 0")))?;
            let skip = match HeapChain::read(slot0) {
                Some(chain) => {
                    self.next_page = chain.next_vpid;
                    chain.is_bestspace()
                }
                None => {
                    self.next_page = header_next_vpid(slot0);
                    false
                }
            };
            if !skip {
                self.page = Some((vpid, page));
                self.slot = 1;
                return Ok(true);
            }
        }
    }

    fn resolve(&self, oid: Oid, rtype: RecordType, rec: &[u8]) -> io::Result<Option<HeapRecord>> {
        resolve_record(self.vs, oid, rtype, rec)
    }

    fn step(&mut self) -> io::Result<Option<HeapRecord>> {
        loop {
            if self.page.is_none() && !self.load_next_page()? {
                return Ok(None);
            }
            let (vpid, page) = self.page.as_ref().unwrap();
            let vpid = *vpid;
            let sp = slotted(page, vpid)?;
            while self.slot < sp.num_slots() {
                let slot = self.slot;
                self.slot += 1;
                if let Some((rtype, rec)) = sp.record(slot) {
                    let oid = Oid::new(vpid.volid, vpid.pageid, slot as i16);
                    if let Some(r) = self.resolve(oid, rtype, rec)? {
                        return Ok(Some(r));
                    }
                }
            }
            self.page = None;
        }
    }
}

/// The whole record of the object whose home slot `oid` holds `rec` of type `rtype`: REC_HOME as is, REC_RELOCATION
/// resolved to its REC_NEWHOME, REC_BIGONE assembled from its overflow pages; `None` for any other slot type.
pub fn resolve_record(
    vs: &VolumeSet,
    oid: Oid,
    rtype: RecordType,
    rec: &[u8],
) -> io::Result<Option<HeapRecord>> {
    Ok(Some(match rtype {
        RecordType::Home => HeapRecord {
            oid,
            kind: HomeKind::Home,
            bytes: rec.to_vec(),
        },
        RecordType::Relocation => {
            let newhome = Oid::read(rec, 0);
            let page = vs.read_page(newhome.vpid())?;
            let (t, bytes) = slotted(&page, newhome.vpid())?
                .record(newhome.slotid as usize)
                .ok_or_else(|| invalid(format!("{oid:?}: relocation to empty slot {newhome:?}")))?;
            if t != RecordType::NewHome {
                return Err(invalid(format!(
                    "{oid:?}: relocation target {newhome:?} is {t:?}"
                )));
            }
            HeapRecord {
                oid,
                kind: HomeKind::Relocated { newhome },
                bytes: bytes.to_vec(),
            }
        }
        RecordType::BigOne => {
            let first_overflow = Oid::read(rec, 0).vpid();
            HeapRecord {
                oid,
                kind: HomeKind::Big { first_overflow },
                bytes: read_overflow(vs, first_overflow)?,
            }
        }
        _ => return Ok(None),
    }))
}

impl Iterator for HeapScan<'_> {
    type Item = io::Result<HeapRecord>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        match self.step() {
            Ok(r) => r.map(Ok),
            Err(e) => {
                self.failed = true;
                Some(Err(e))
            }
        }
    }
}
