//! Rewrites one heap file of the target in place from 11.4 to develop: the pages that need it are patched or
//! re-encoded, then the header page becomes develop's with its bestspace pages.
//!
//! Input: a [`SpaceManager`] over the target (a copy of the source volumes), the source's [`VacuumHorizon`] and a
//! [`HeapJob`]. Output: the heap rewritten; a [`HeapReport`] with what changed and the statistics written.
//! Which pages are read and written (workspace#499, replacing R2 § 3 R5 "reset every page"): a re-encoded class
//! walks and rewrites its whole chain; otherwise the chain is walked only when the source had vacuum work pending,
//! and only patch targets are written; the header page is always rewritten. Pages not written stay byte-identical
//! (clone sharing kept); their `max_mvccid` is below the target's first MVCCID because the new log continues the
//! source's `mvcc_next_id` (workspace#484 invariant, `heap_file.c:25045` assert).
//! Source: workspace#482 decision 10 (patch/transform after releases, before index builds), R2 § 3 R1-R7.

use std::io;

use crate::file_alloc::SpaceManager;
use crate::file_header::{FileHeader, FileType};
use crate::heap_rewrite::header_rewrite::{HeaderOutcome, rewrite_header};
use crate::heap_rewrite::heap_state::{HeapState, PageStats, invalid};
use crate::heap_rewrite::page_rewrite::{PageCounts, PageMode, rewrite_current_page};
use crate::heap_rewrite::reencode::Reencode;
use crate::heap_rewrite::vacuum_horizon::VacuumHorizon;
use crate::page::db_area;
use crate::record::heap_chain::HeapChain;
use crate::record::heap_hdr_stats::HeapHdrStats;
use crate::spage::SlottedPage;
use crate::{Hfid, Oid, VersionLine, Vfid, Vpid};

/// One heap to rewrite.
#[derive(Clone, Copy)]
pub struct HeapJob<'a> {
    pub hfid: Hfid,
    /// Records go through this re-encoder (a re-encoded class, or a catalog class whose rows are regenerated);
    /// `None` keeps record bytes and only removes vacuum residue.
    pub reencode: Option<&'a dyn Reencode>,
    /// The root class heap (one bestspace shard).
    pub root: bool,
}

#[derive(Clone, Debug)]
pub struct HeapReport {
    pub hfid: Hfid,
    pub class_oid: Oid,
    /// Every chain page was read; the statistics are measured (`plan.json`, G3 comparison condition).
    pub measured: bool,
    /// Heap pages read (chain pages, NEWHOME targets, appended pages).
    pub pages_read: u64,
    /// Pages whose records were patched or re-encoded (header page included).
    pub pages_rewritten: u64,
    pub counts: PageCounts,
    /// Pages added to the chain for NEWHOME records.
    pub appended_pages: u64,
    pub header: HeaderOutcome,
    /// The overflow file after the rewrite (created when a record became big and the heap had none).
    pub ovf_vfid: Vfid,
    pub stats: PageStats,
}

fn chain_of(page: &[u8], vpid: Vpid) -> io::Result<HeapChain> {
    SlottedPage::new(db_area(page))
        .and_then(|sp| sp.record(0))
        .and_then(|(_, r)| HeapChain::read(r))
        .ok_or_else(|| invalid(format!("heap page {vpid:?}: slot 0 is not a chain record")))
}

/// Rewrites heap `job.hfid` of the target `space` (11.4 layout in, develop layout out).
pub fn rewrite_heap(
    space: &SpaceManager,
    horizon: &VacuumHorizon,
    job: &HeapJob,
) -> io::Result<HeapReport> {
    let vs = space.volumes();
    let hv = job.hfid.vpid();
    let hp = vs.read_page(hv)?;
    let rec0 = SlottedPage::new(db_area(&hp))
        .and_then(|sp| sp.record(0))
        .ok_or_else(|| invalid(format!("heap header {hv:?} has no slot 0")))?
        .1;
    let src = HeapHdrStats::read(rec0, VersionLine::V11_4)
        .map_err(|e| invalid(format!("heap header {hv:?}: {e}")))?;
    let reuse_slots = FileHeader::read(&vs.read_page(job.hfid.vfid.header_vpid())?).file_type()
        == Some(FileType::HeapReuseSlots);
    let mut st = HeapState::new(
        space,
        job.hfid,
        src.class_oid,
        reuse_slots,
        src.ovf_vfid,
        src.last_vpid,
    );
    st.header_reserve =
        HeapHdrStats::size(VersionLine::Develop) - HeapHdrStats::size(VersionLine::V11_4);

    let walk = job.reencode.is_some() || horizon.pending;
    let mut counts = PageCounts::default();
    let mut rewritten = 0;
    // the header page is read first: NEWHOME first-fit order is read order
    st.load(hv)?;
    if walk {
        let stop = src.last_vpid;
        let limit: u64 = vs
            .volumes()
            .iter()
            .map(|v| v.header.nsect_total.max(0) as u64 * v.header.sect_npgs.max(0) as u64)
            .sum();
        let mut v = src.next_vpid;
        let mut walked = 0u64;
        while !v.is_null() {
            walked += 1;
            if walked > limit {
                return Err(invalid(format!(
                    "heap {hv:?}: chain longer than the volume set"
                )));
            }
            st.load(v)?;
            let chain = st.peek(v, |p| chain_of(p, v))?;
            let mode = match job.reencode {
                Some(re) => Some(PageMode::Reencode(re)),
                None if horizon.page_needs_patch(&chain) => Some(PageMode::Patch),
                None => None,
            };
            if let Some(mode) = mode {
                counts.add(rewrite_current_page(&mut st, mode)?);
                rewritten += 1;
            }
            if v == stop {
                break;
            }
            v = chain.next_vpid;
        }
    }
    st.load(hv)?;
    counts.add(rewrite_current_page(
        &mut st,
        job.reencode.map_or(PageMode::Patch, PageMode::Reencode),
    )?);
    rewritten += 1;
    let header = rewrite_header(&mut st, &src, walk, job.root)?;
    st.flush()?;
    Ok(HeapReport {
        hfid: job.hfid,
        class_oid: src.class_oid,
        measured: walk,
        pages_read: st.pages().len() as u64,
        pages_rewritten: rewritten,
        counts,
        appended_pages: st.appended_pages,
        stats: header.stats,
        header,
        ovf_vfid: st.ovf_vfid,
    })
}
