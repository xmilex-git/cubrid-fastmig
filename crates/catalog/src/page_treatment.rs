//! The page treatment of the user heaps on the 11.4 → develop path: every heap gets develop's header and bestspace
//! pages; a re-encoded class has every record rewritten (transform), any other class only its patch target pages
//! (patch). Two passes (workspace#536): the isolated pages of every heap in parallel, sector ranges of all heaps in
//! one queue (`rewrite_isolated_pages`); then each heap's chain walk for the rest, one heap per worker at a time.
//!
//! Input: the target's [`SpaceManager`], the [`SourceCatalog`], the source's [`VacuumHorizon`], the worker limit and
//! the meter of the page-treatment phase (unit: pages of the walked heaps, workspace#508). Output: one [`HeapReport`] per user
//! heap, in class order.
//! Source: workspace#482 decision 10.3 (after the old files are freed, before the index builds); workspace#499
//! hand-over (`rewrite_heap` per heap with `ClassReEncoder` for a re-encoded class; heaps are independent because the
//! `SpaceManager` serializes space changes); workspace#481 (one worker limit for the run); workspace#507 (LOB
//! locators of a re-encoded class move to develop's per-attribute directories).

use std::collections::HashSet;
use std::io;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use fastmig_ondisk::file_alloc::SpaceManager;
use fastmig_ondisk::file_table::FileTable;
use fastmig_ondisk::heap_rewrite::{
    ClassReEncoder, Growth, HeapJob, HeapReport, ReEncode, VacuumHorizon, rewrite_heap,
    rewrite_isolated_pages,
};
use fastmig_ondisk::page::{self, PageType};
use fastmig_ondisk::record::heap_chain::HeapChain;
use fastmig_ondisk::record::heap_scan::resolve_record;
use fastmig_ondisk::spage::SlottedPage;
use fastmig_ondisk::volume_set::VolumeSet;
use fastmig_ondisk::{Hfid, Oid, Vpid};
use fastmig_progress::Meter;

use crate::classify::ClassKind;
use crate::lob_dir_heap::lob_dir_heap;
use crate::source_catalog::SourceCatalog;

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

pub fn treat_user_heaps(
    space: &SpaceManager,
    source: &SourceCatalog,
    horizon: &VacuumHorizon,
    lob_base: Option<&Path>,
    worker_limit: usize,
    meter: Meter,
) -> io::Result<Vec<HeapReport>> {
    let classes: Vec<usize> = (0..source.classes.len())
        .filter(|&i| {
            source.plans[i].kind == ClassKind::User && !source.classes[i].class.heap.is_null()
        })
        .collect();
    let re_encoders = classes
        .iter()
        .map(|&i| encoder_of(source, lob_base, i))
        .collect::<io::Result<Vec<_>>>()?;
    let pages_walked = |n| meter.add(n);
    let mut jobs: Vec<HeapJob> = classes
        .iter()
        .zip(&re_encoders)
        .map(|(&i, r)| HeapJob {
            hfid: source.classes[i].class.heap,
            source_line: source.line,
            re_encode: r.as_ref().map(|r| r as _),
            root: false,
            pages_walked: Some(&pages_walked),
            isolated: None,
        })
        .collect();
    let isolated = rewrite_isolated_pages(space, horizon, &jobs, worker_limit)?;
    for (i, j) in jobs.iter_mut().enumerate() {
        j.isolated = Some(isolated.heap(i));
    }
    let next = AtomicUsize::new(0);
    let reports: Mutex<Vec<Option<io::Result<HeapReport>>>> =
        Mutex::new((0..jobs.len()).map(|_| None).collect());
    std::thread::scope(|s| {
        for _ in 0..worker_limit.clamp(1, jobs.len().max(1)) {
            s.spawn(|| {
                loop {
                    let j = next.fetch_add(1, Ordering::Relaxed);
                    let Some(job) = jobs.get(j) else { break };
                    let r = rewrite_heap(space, horizon, job).map_err(|e| {
                        invalid(format!("{} heap: {e}", source.plans[classes[j]].name))
                    });
                    reports.lock().unwrap()[j] = Some(r);
                }
            });
        }
    });
    reports
        .into_inner()
        .unwrap()
        .into_iter()
        .map(|r| r.unwrap_or_else(|| Err(invalid("heap not treated".into()))))
        .collect()
}

/// The encoder of class `i` when it is re-encoded: user classes are MVCC classes; LOB locators move to develop's
/// per-attribute directories (workspace#507).
pub fn encoder_of(
    source: &SourceCatalog,
    lob_base: Option<&Path>,
    i: usize,
) -> io::Result<Option<ClassReEncoder>> {
    let c = &source.classes[i];
    let plan = &source.plans[i];
    plan.re_encoded
        .then(|| ClassReEncoder::new(&c.class, source.line, source.target, true))
        .transpose()
        .map(|r| {
            r.map(|r| {
                r.with_lob_dirs(
                    lob_dir_heap(c, &source.classes),
                    lob_base.map(Path::to_path_buf),
                )
            })
        })
        .map_err(|e| invalid(format!("{}: {e}", plan.name)))
}

/// Pages the page treatment may append to the user heaps of re-encoded classes not in `counted` (whose records the
/// index pair count re-encoded already, `pair_count::pair_stats_and_growth`), an upper bound (workspace#526: the
/// target space the run writes outside what it takes at the start). Only a re-encoded record can grow (a patch drops
/// header bytes): each record is re-encoded as the treatment will and counted by [`Growth`]. A record the encoder
/// cannot read counts twice its size. The pages are re-encoded a sector at a time from one queue over every heap, so
/// one big heap spreads over the workers.
pub fn appended_pages_bound(
    vols: &VolumeSet,
    source: &SourceCatalog,
    lob_base: Option<&Path>,
    worker_limit: usize,
    counted: &HashSet<Oid>,
) -> io::Result<u64> {
    let mut encoders = Vec::new();
    for i in 0..source.classes.len() {
        if source.plans[i].kind == ClassKind::User
            && source.plans[i].re_encoded
            && !source.classes[i].class.heap.is_null()
            && !counted.contains(&source.classes[i].oid)
            && let Some(enc) = encoder_of(source, lob_base, i)?
        {
            encoders.push((source.classes[i].class.heap, enc));
        }
    }
    // the user pages of every heap by sector, in one queue: a big heap is no longer one worker's (workspace#526)
    let mut units: Vec<(usize, Vec<Vpid>)> = Vec::new();
    for (k, (hfid, _)) in encoders.iter().enumerate() {
        for (_, pages) in FileTable::read(vols, hfid.vfid)?.user_pages_by_sector() {
            units.push((k, pages));
        }
    }
    let usable = Growth::usable(vols.io_pagesize());
    let grown: Vec<Mutex<Growth>> = encoders.iter().map(|_| Mutex::default()).collect();
    let next = AtomicUsize::new(0);
    let first_err: Mutex<Option<io::Error>> = Mutex::new(None);
    std::thread::scope(|s| {
        for _ in 0..worker_limit.clamp(1, units.len().max(1)) {
            s.spawn(|| {
                let mut buf = vec![0u8; vols.io_pagesize()];
                loop {
                    let j = next.fetch_add(1, Ordering::Relaxed);
                    let Some((k, pages)) = units.get(j) else {
                        break;
                    };
                    let (hfid, enc) = &encoders[*k];
                    match pages_growth(vols, *hfid, enc, pages, usable, &mut buf) {
                        Ok(g) => grown[*k].lock().unwrap().merge(&g),
                        Err(e) => {
                            first_err.lock().unwrap().get_or_insert(e);
                        }
                    }
                }
            });
        }
    });
    if let Some(e) = first_err.into_inner().unwrap() {
        return Err(e);
    }
    Ok(grown.iter().map(|g| g.lock().unwrap().pages(usable)).sum())
}

/// The grown records of `pages` of heap `hfid`: bytes of the small ones packed (aligned, with their slot) and pages
/// of the big ones.
fn pages_growth(
    vols: &VolumeSet,
    hfid: Hfid,
    enc: &ClassReEncoder,
    pages: &[Vpid],
    usable: usize,
    buf: &mut [u8],
) -> io::Result<Growth> {
    let mut g = Growth::default();
    for &vpid in pages {
        vols.read_page_into(vpid, buf)?;
        if page::ptype(buf) != Some(PageType::Heap) {
            continue;
        }
        let Some(sp) = SlottedPage::new(page::db_area(buf)) else {
            continue;
        };
        if vpid != hfid.vpid()
            && sp
                .record(0)
                .and_then(|(_, r)| HeapChain::read(r))
                .is_some_and(|c| c.is_bestspace())
        {
            continue;
        }
        for slot in 1..sp.num_slots() {
            let Some((rtype, rec)) = sp.record(slot) else {
                continue;
            };
            let oid = Oid::new(vpid.volid, vpid.pageid, slot as i16);
            let Some(r) = resolve_record(vols, oid, rtype, rec)? else {
                continue;
            };
            let old = r.bytes.len();
            g.add(
                old,
                enc.re_encode(oid, &r.bytes).map_or(2 * old, |b| b.len()),
                usable,
            );
        }
    }
    Ok(g)
}
