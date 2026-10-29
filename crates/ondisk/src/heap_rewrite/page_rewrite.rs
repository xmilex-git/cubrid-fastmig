//! Rewrites the records whose home is the current heap page: vacuum-residue removal (patch) or re-encoding, then
//! the page's slotted area compacted, the 11.4-only header bit cleared and the chain record reset.
//!
//! Input: a [`HeapState`] whose current page is the page to rewrite, and the [`PageTreatment`]. Output: the page (and the
//! NEWHOME/overflow pages its records reach) edited; counts in [`PageCounts`].
//! Source: R2 § 3 R3 (record rules; a REC_NEWHOME is reached from its home, never on its own; slots are never
//! removed), R4 (`spage_compact` result, `need_update_best_hint` cleared), R5 (chain `max_mvccid` = 0, vacuum
//! status bits cleared, workspace#501; applied to rewritten pages only, workspace#499 replaces "every page"), R7 (re-encoding); engine `vacuum.c`
//! `vacuum_heap_record` (:2190-2440: a deleted REC_RELOCATION also deletes its NEWHOME as
//! REC_DELETED_WILL_REUSE, a deleted REC_BIGONE releases its overflow pages), `spage_vacuum_slot`; develop
//! @5b1867174, same in release/11.4 @46212adcd.

use std::io;

use crate::heap_rewrite::heap_state::{HeapState, editor, invalid, spage_err};
use crate::heap_rewrite::overflow;
use crate::heap_rewrite::placement::{self, Released, home_slot, slot_record};
use crate::heap_rewrite::re_encode::ReEncode;
use crate::heap_rewrite::record_patch::{
    OVERFLOW_FIRST_DATA, reset_overflow_header, strip_insid_and_prev,
};
use crate::page::{db_area, db_area_mut};
use crate::record::HomeKind;
use crate::record::heap_chain::HeapChain;
use crate::record::heap_scan::read_overflow;
use crate::record::mvcc_header::RecordHeader;
use crate::spage::{RecordType, SlottedPage};
use crate::{Oid, Vpid};

#[derive(Clone, Copy)]
pub enum PageTreatment<'a> {
    /// Remove vacuum residue, bytes otherwise kept.
    Patch,
    /// Write every live record again through the re-encoder.
    Transform(&'a dyn ReEncode),
}

/// What a rewrite did, summed over pages. Every transformed record counts in exactly one of `same_slot_*`,
/// `relocated`, `big`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PageCounts {
    /// Committed deletes removed (home slot vacuumed).
    pub deleted: u64,
    /// Records whose INSID / previous-version LSA was dropped, or whose overflow header was reset.
    pub cleaned: u64,
    /// Transformed records back in their home slot, by new length against the old record's.
    pub same_slot_shrunk: u64,
    pub same_slot_same: u64,
    pub same_slot_grown: u64,
    /// Transformed records now REC_RELOCATION with a new REC_NEWHOME.
    pub relocated: u64,
    /// Transformed records now REC_BIGONE with a new overflow chain.
    pub big: u64,
    /// Old REC_NEWHOME slots deleted (re-placed or deleted objects).
    pub old_newhome_released: u64,
    /// Old overflow chains released and their pages.
    pub old_overflow_chains_released: u64,
    pub overflow_pages_released: u64,
    pub overflow_pages_written: u64,
}

impl PageCounts {
    pub fn add(&mut self, o: PageCounts) {
        self.deleted += o.deleted;
        self.cleaned += o.cleaned;
        self.same_slot_shrunk += o.same_slot_shrunk;
        self.same_slot_same += o.same_slot_same;
        self.same_slot_grown += o.same_slot_grown;
        self.relocated += o.relocated;
        self.big += o.big;
        self.old_newhome_released += o.old_newhome_released;
        self.old_overflow_chains_released += o.old_overflow_chains_released;
        self.overflow_pages_released += o.overflow_pages_released;
        self.overflow_pages_written += o.overflow_pages_written;
    }

    fn released(&mut self, r: Released) {
        self.old_newhome_released += r.newhome as u64;
        self.old_overflow_chains_released += (r.overflow_pages > 0) as u64;
        self.overflow_pages_released += r.overflow_pages as u64;
    }
}

fn header_of(oid: Oid, rec: &[u8]) -> io::Result<RecordHeader> {
    RecordHeader::read(rec).map_err(|e| invalid(format!("{oid:?}: {e}")))
}

fn vacuum(st: &mut HeapState, oid: Oid, reusable: bool) -> io::Result<()> {
    st.edit(oid.vpid(), |p| {
        editor(p, oid.vpid())?
            .vacuum_slot(oid.slotid as usize, reusable)
            .map_err(|e| spage_err(oid.vpid(), e))
    })
}

fn update(st: &mut HeapState, oid: Oid, rec: &[u8]) -> io::Result<()> {
    st.edit(oid.vpid(), |p| {
        editor(p, oid.vpid())?
            .update(oid.slotid as usize, rec)
            .map_err(|e| spage_err(oid.vpid(), e))
    })
}

/// The record bytes an object's home slot leads to, and the object's MVCC header.
enum Target {
    Home(Vec<u8>),
    Relocated(Oid, Vec<u8>),
    Big(Vpid),
}

fn patch_object(
    st: &mut HeapState,
    oid: Oid,
    target: Target,
    c: &mut PageCounts,
) -> io::Result<()> {
    let reuse = st.reuse_slots;
    match target {
        Target::Home(rec) => {
            if header_of(oid, &rec)?.is_deleted() {
                vacuum(st, oid, reuse)?;
                c.deleted += 1;
            } else if let Some(b) =
                strip_insid_and_prev(&rec).map_err(|e| invalid(format!("{oid:?}: {e}")))?
            {
                update(st, oid, &b)?;
                c.cleaned += 1;
            }
        }
        Target::Relocated(nh, rec) => {
            if header_of(nh, &rec)?.is_deleted() {
                vacuum(st, nh, true)?;
                vacuum(st, oid, reuse)?;
                c.deleted += 1;
                c.old_newhome_released += 1;
            } else if let Some(b) =
                strip_insid_and_prev(&rec).map_err(|e| invalid(format!("{nh:?}: {e}")))?
            {
                update(st, nh, &b)?;
                c.cleaned += 1;
            }
        }
        Target::Big(first) => {
            let vs = st.vs();
            let mut p = vs.read_page(first)?;
            let h = header_of(oid, &db_area(&p)[OVERFLOW_FIRST_DATA..])?;
            if h.is_deleted() {
                c.released(Released {
                    newhome: false,
                    overflow_pages: overflow::release_chain(st.space, st.ovf_vfid, first)?,
                });
                vacuum(st, oid, reuse)?;
                c.deleted += 1;
            } else if h.mvcc_flags != 0 {
                let before = p.clone();
                reset_overflow_header(db_area_mut(&mut p))
                    .map_err(|e| invalid(format!("{oid:?}: {e}")))?;
                if p != before {
                    vs.write_page(first, &p)?;
                    c.cleaned += 1;
                }
            }
        }
    }
    Ok(())
}

fn re_encode_object(
    st: &mut HeapState,
    oid: Oid,
    target: Target,
    re: &dyn ReEncode,
    c: &mut PageCounts,
) -> io::Result<()> {
    let (deleted, rec) = match &target {
        Target::Home(rec) | Target::Relocated(_, rec) => {
            (header_of(oid, rec)?.is_deleted(), rec.clone())
        }
        Target::Big(first) => {
            let rec = read_overflow(st.vs(), *first)?;
            (header_of(oid, &rec)?.is_deleted(), rec)
        }
    };
    if deleted {
        c.released(placement::release_target(st, oid)?);
        vacuum(st, oid, st.reuse_slots)?;
        c.deleted += 1;
        return Ok(());
    }
    let new = re
        .re_encode(oid, &rec)
        .map_err(|e| invalid(format!("{oid:?}: re-encode: {e}")))?;
    let placed = placement::place(st, oid, &new, re.mvcc())?;
    c.released(placed.released);
    c.overflow_pages_written += placed.overflow_pages_written as u64;
    match placed.kind {
        HomeKind::Home if new.len() < rec.len() => c.same_slot_shrunk += 1,
        HomeKind::Home if new.len() == rec.len() => c.same_slot_same += 1,
        HomeKind::Home => c.same_slot_grown += 1,
        HomeKind::Relocated { .. } => c.relocated += 1,
        HomeKind::Big { .. } => c.big += 1,
    }
    Ok(())
}

/// Rewrites the current page's records under `treatment`, then compacts it, clears the 11.4 header bit and, on a data
/// page, resets the chain record.
pub(crate) fn rewrite_current_page(
    st: &mut HeapState,
    treatment: PageTreatment,
) -> io::Result<PageCounts> {
    let vpid = st.current_vpid();
    let nslots = st.peek(vpid, |p| {
        Ok(SlottedPage::new(db_area(p)).map_or(0, |sp| sp.num_slots()))
    })?;
    let mut c = PageCounts::default();
    for slot in 1..nslots {
        let oid = Oid::new(vpid.volid, vpid.pageid, slot as i16);
        let Some((t, body)) = slot_record(st, oid)? else {
            continue; // deleted or address-only slot
        };
        let target = match t {
            RecordType::Home => Target::Home(body),
            RecordType::Relocation => {
                let nh = Oid::read(&body, 0);
                let (nt, rec) = home_slot(st, nh)?;
                if nt != RecordType::NewHome {
                    return Err(invalid(format!(
                        "{oid:?}: relocation target {nh:?} is {nt:?}"
                    )));
                }
                Target::Relocated(nh, rec)
            }
            RecordType::BigOne => Target::Big(Oid::read(&body, 0).vpid()),
            _ => continue,
        };
        match treatment {
            PageTreatment::Patch => patch_object(st, oid, target, &mut c)?,
            PageTreatment::Transform(re) => re_encode_object(st, oid, target, re, &mut c)?,
        }
    }
    let header_page = vpid == st.hfid.vpid();
    st.edit(vpid, |p| {
        let mut ed = editor(p, vpid)?;
        ed.compact().map_err(|e| spage_err(vpid, e))?;
        ed.clear_need_update_best_hint();
        if !header_page {
            let mut rec = ed
                .view()
                .record(0)
                .map(|(_, r)| r.to_vec())
                .unwrap_or_default();
            if let Some(mut chain) = HeapChain::read(&rec) {
                chain.max_mvccid = 0;
                chain.clear_vacuum_status();
                chain.write(&mut rec);
                ed.update(0, &rec).map_err(|e| spage_err(vpid, e))?;
            }
        }
        Ok(())
    })?;
    Ok(c)
}
