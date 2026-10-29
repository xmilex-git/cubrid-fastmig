//! The page treatment of the user heaps on the 11.4 → develop path: every heap gets develop's header and bestspace
//! pages; a re-encoded class has every record rewritten (transform), any other class only its patch target pages
//! (patch). Heaps run in parallel, one heap per worker at a time.
//!
//! Input: the target's [`SpaceManager`], the [`SourceCatalog`], the source's [`VacuumHorizon`], the worker limit and
//! the meter of the page-treatment phase (unit: records of re-encoded classes). Output: one [`HeapReport`] per user
//! heap, in class order.
//! Source: workspace#482 decision 10.3 (after the old files are freed, before the index builds); workspace#499
//! hand-over (`rewrite_heap` per heap with `ClassReEncoder` for a re-encoded class; heaps are independent because the
//! `SpaceManager` serializes space changes); workspace#481 (one worker limit for the run).

use std::io;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use fastmig_ondisk::VersionLine;
use fastmig_ondisk::file_alloc::SpaceManager;
use fastmig_ondisk::heap_rewrite::{
    ClassReEncoder, HeapJob, HeapReport, VacuumHorizon, rewrite_heap,
};
use fastmig_progress::Meter;

use crate::classify::ClassKind;
use crate::source_catalog::SourceCatalog;

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

pub fn treat_user_heaps(
    space: &SpaceManager,
    source: &SourceCatalog,
    horizon: &VacuumHorizon,
    worker_limit: usize,
    meter: Meter,
) -> io::Result<Vec<HeapReport>> {
    let jobs: Vec<usize> = (0..source.classes.len())
        .filter(|&i| {
            source.plans[i].kind == ClassKind::User && !source.classes[i].class.heap.is_null()
        })
        .collect();
    let next = AtomicUsize::new(0);
    let reports: Mutex<Vec<Option<io::Result<HeapReport>>>> =
        Mutex::new((0..jobs.len()).map(|_| None).collect());
    std::thread::scope(|s| {
        for _ in 0..worker_limit.clamp(1, jobs.len().max(1)) {
            s.spawn(|| {
                loop {
                    let j = next.fetch_add(1, Ordering::Relaxed);
                    let Some(&i) = jobs.get(j) else { break };
                    let r = treat_one(space, source, horizon, i, meter);
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

fn treat_one(
    space: &SpaceManager,
    source: &SourceCatalog,
    horizon: &VacuumHorizon,
    i: usize,
    meter: Meter,
) -> io::Result<HeapReport> {
    let c = &source.classes[i];
    let plan = &source.plans[i];
    // user classes are MVCC classes
    let re_encoder = plan
        .re_encoded
        .then(|| ClassReEncoder::new(&c.class, true))
        .transpose()
        .map_err(|e| invalid(format!("{}: {e}", plan.name)))?;
    let report = rewrite_heap(
        space,
        horizon,
        &HeapJob {
            hfid: c.class.heap,
            source_line: VersionLine::V11_4,
            re_encode: re_encoder.as_ref().map(|r| r as _),
            root: false,
        },
    )
    .map_err(|e| invalid(format!("{} heap: {e}", plan.name)))?;
    if re_encoder.is_some() {
        let k = &report.counts;
        meter.add(k.same_slot_shrunk + k.same_slot_same + k.same_slot_grown + k.relocated + k.big);
    }
    Ok(report)
}
