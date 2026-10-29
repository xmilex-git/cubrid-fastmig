//! Rewrites one heap file of the target in place into develop's format: the pages that need it are patched or
//! re-encoded, then the header page becomes (11.4 source) or stays (develop source) develop's with its bestspace pages.
//!
//! Input: a [`SpaceManager`] over the target (a copy of the source volumes), the source's [`VacuumHorizon`] and a
//! [`HeapJob`]. Output: the heap rewritten; a [`HeapReport`] with what changed and the statistics written.
//! Which pages are read and written (workspace#499, replacing R2 § 3 R5 "reset every page"): a re-encoded class
//! walks and rewrites its whole chain; otherwise the chain is walked only when the source had vacuum work pending,
//! and only patch targets are written; the header page is always rewritten. Pages not written stay byte-identical
//! (clone sharing kept); their `max_mvccid` is below the target's first MVCCID because the new log continues the
//! source's `mvcc_next_id` (workspace#484 invariant, `heap_file.c:25045` assert).
//! A develop source (the same-version path, workspace#501) has its bestspace pages in the chain already: they are
//! never patched nor counted, and a heap that is not walked is left byte-identical, header included.
//! Source: workspace#482 decision 10 (patch/transform after releases, before index builds), R2 § 3 R1-R7.

use std::io;

use crate::file_alloc::SpaceManager;
use crate::file_header::{FileHeader, FileType};
use crate::heap_rewrite::header_rewrite::{HeaderOutcome, rewrite_header};
use crate::heap_rewrite::heap_state::{HeapState, PageStats, chain_of, invalid};
use crate::heap_rewrite::page_rewrite::{PageCounts, PageTreatment, rewrite_current_page};
use crate::heap_rewrite::re_encode::ReEncode;
use crate::heap_rewrite::vacuum_horizon::VacuumHorizon;
use crate::page::db_area;
use crate::record::heap_hdr_stats::{HeapHdrStats, LineFields};
use crate::spage::SlottedPage;
use crate::{Hfid, Oid, VersionLine, Vfid, Vpid};

/// One heap to rewrite.
#[derive(Clone, Copy)]
pub struct HeapJob<'a> {
    pub hfid: Hfid,
    /// Release line of the source volumes: the header record's layout and whether bestspace pages exist.
    pub source_line: VersionLine,
    /// Records go through this re-encoder (a re-encoded class, or a catalog class whose rows are regenerated);
    /// `None` keeps record bytes and only removes vacuum residue.
    pub re_encode: Option<&'a dyn ReEncode>,
    /// The root class heap (one bestspace shard).
    pub root: bool,
    /// Called with 1 for every page of a walked heap (header and chain pages, the heap file's user pages): the
    /// page-treatment progress, whose total the plan sums from the same pages (workspace#508).
    pub pages_walked: Option<&'a dyn Fn(u64)>,
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

/// Rewrites heap `job.hfid` of the target `space` (`job.source_line` layout in, develop layout out).
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
    let src = HeapHdrStats::read(rec0, job.source_line)
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
        HeapHdrStats::size(VersionLine::Develop) - HeapHdrStats::size(job.source_line);
    let bestspace_pages: &[Vpid] = match &src.line {
        LineFields::Develop(d) => &d.pages,
        LineFields::V11_4(_) => &[],
    };

    let walk = job.re_encode.is_some() || horizon.pending;
    if !walk && job.source_line == VersionLine::Develop {
        // nothing to clean and nothing to convert: header and pages stay as the source wrote them (workspace#501)
        return Ok(untouched(job, &src, bestspace_pages));
    }
    let mut counts = PageCounts::default();
    let mut rewritten = 0;
    // the header page is read first: NEWHOME first-fit order is read order
    st.load(hv)?;
    let page_walked = || {
        if let Some(f) = job.pages_walked {
            f(1)
        }
    };
    if walk {
        page_walked();
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
            page_walked();
            if walked > limit {
                return Err(invalid(format!(
                    "heap {hv:?}: chain longer than the volume set"
                )));
            }
            if bestspace_pages.contains(&v) {
                // entries, not records: neither patched nor read into the statistics (heap_update_statistics)
                v = st.peek(v, |p| chain_of(p, v))?.next_vpid;
                continue;
            }
            st.load(v)?;
            let chain = st.peek(v, |p| chain_of(p, v))?;
            let treatment = match job.re_encode {
                Some(re) => Some(PageTreatment::Transform(re)),
                None if horizon.page_needs_patch(&chain) => Some(PageTreatment::Patch),
                None => None,
            };
            if let Some(treatment) = treatment {
                counts.add(rewrite_current_page(&mut st, treatment)?);
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
        job.re_encode
            .map_or(PageTreatment::Patch, PageTreatment::Transform),
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

/// The report of a develop heap left as is: only its header page was read.
fn untouched(job: &HeapJob, src: &HeapHdrStats, bestspace_pages: &[Vpid]) -> HeapReport {
    let stats = PageStats {
        pages: src.num_pages.max(0) as u64,
        recs: src.num_recs,
        sumlen: src.recs_sumlen,
    };
    HeapReport {
        hfid: job.hfid,
        class_oid: src.class_oid,
        measured: false,
        pages_read: 1,
        pages_rewritten: 0,
        counts: PageCounts::default(),
        appended_pages: 0,
        header: HeaderOutcome {
            bestspace_pages: bestspace_pages.to_vec(),
            evicted: 0,
            stats,
        },
        ovf_vfid: src.ovf_vfid,
        stats,
    }
}
