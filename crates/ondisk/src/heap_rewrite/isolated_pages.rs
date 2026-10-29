//! The parallel first pass of the page treatment: every isolated page of many heaps rewritten at once, before each
//! heap's chain walk ([`rewrite_heap`]) does the rest in chain order.
//!
//! Input: the target's [`SpaceManager`], the source's [`VacuumHorizon`], the [`HeapJob`]s and a worker count.
//! Output: per job its [`IsolatedPages`] (the pages written and their counts); [`rewrite_heap`] then reads those pages
//! without rewriting them. Work units: a heap's user pages (file table, no chain walk) cut into ranges of
//! `UNIT_SECTORS` sectors, heaps largest first, every unit in one queue the workers drain — one large heap spreads
//! over every worker (the index heap scan's shape, workspace#503).
//! Why the target bytes equal the chain walk alone (workspace#536): an isolated page has no REC_RELOCATION,
//! REC_BIGONE or REC_NEWHOME slot, is not the chain's last page, and its rewrite edits no other page. The chain walk
//! adds a page to its first-fit candidates only when it reads it, releases a NEWHOME slot on it, or links a new page
//! after it (the last page); none of these reaches an isolated page, so nothing touches it before its turn, and at its
//! turn every decision depends on that page alone. A page whose rewrite would reach another page (a record grown past
//! its slot, an overflow chain) is dropped unwritten and left to the chain walk, so every page allocation stays in
//! chain order, independent of worker timing.
//! Source: workspace#536 (measured: one 44,411-page heap held the 11.4 → develop phase at 17 s on any worker count).
//!
//! [`rewrite_heap`]: crate::heap_rewrite::rewrite_heap

use std::io;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use crate::file_alloc::SpaceManager;
use crate::file_header::{FileHeader, FileType};
use crate::file_table::FileTable;
use crate::heap_rewrite::heap_state::{HeapState, chain_of, invalid, is_left_to_chain_walk};
use crate::heap_rewrite::page_rewrite::{PageCounts, PageTreatment, rewrite_current_page};
use crate::heap_rewrite::rewrite_heap::HeapJob;
use crate::heap_rewrite::vacuum_horizon::VacuumHorizon;
use crate::page::db_area;
use crate::record::heap_hdr_stats::{HeapHdrStats, LineFields};
use crate::spage::{RecordType, SlottedPage};
use crate::{Oid, Vpid};

/// Sectors per work unit, as the index heap scan (64 pages each at 16 KiB).
const UNIT_SECTORS: usize = 4;

/// What the first pass wrote: one bit per page of the volume set (1 bit, not a page list, so a large database costs
/// 8 MiB per TiB) and, per job, how many isolated pages and what their rewrite did.
pub struct IsolatedPages {
    bits: Vec<Vec<AtomicU64>>,
    heaps: Vec<(u64, PageCounts)>,
}

impl IsolatedPages {
    fn new(space: &SpaceManager, jobs: usize) -> Self {
        let bits = space
            .volumes()
            .volumes()
            .iter()
            .map(|v| {
                let pages =
                    v.header.nsect_total.max(0) as usize * v.header.sect_npgs.max(0) as usize;
                (0..pages.div_ceil(64)).map(|_| AtomicU64::new(0)).collect()
            })
            .collect();
        IsolatedPages {
            bits,
            heaps: vec![(0, PageCounts::default()); jobs],
        }
    }

    fn word(&self, v: Vpid) -> Option<(&AtomicU64, u64)> {
        let vol = self.bits.get(usize::try_from(v.volid).ok()?)?;
        let p = usize::try_from(v.pageid).ok()?;
        Some((vol.get(p / 64)?, 1 << (p % 64)))
    }

    fn insert(&self, v: Vpid) {
        if let Some((w, bit)) = self.word(v) {
            w.fetch_or(bit, Ordering::Relaxed);
        }
    }

    pub fn contains(&self, v: Vpid) -> bool {
        self.word(v)
            .is_some_and(|(w, bit)| w.load(Ordering::Relaxed) & bit != 0)
    }

    /// Job `i`'s part, for its [`HeapJob::isolated`].
    pub fn heap(&self, i: usize) -> IsolatedHeap<'_> {
        let (pages, counts) = self.heaps[i];
        IsolatedHeap {
            set: self,
            pages,
            counts,
        }
    }
}

/// One heap's isolated pages: the set they are in, how many, and what their rewrite did.
#[derive(Clone, Copy)]
pub struct IsolatedHeap<'a> {
    pub set: &'a IsolatedPages,
    pub pages: u64,
    pub counts: PageCounts,
}

/// What one heap needs for its isolated pages, read once from its header.
struct HeapPlan<'a> {
    job: &'a HeapJob<'a>,
    class_oid: Oid,
    reuse_slots: bool,
    sectors: Vec<Vec<Vpid>>,
}

impl HeapPlan<'_> {
    fn pages(&self) -> usize {
        self.sectors.iter().map(Vec::len).sum()
    }
}

/// Reads heap `job.hfid`'s header and file table: its data pages by sector (header, last and bestspace pages left
/// out).
fn heap_plan<'a>(space: &SpaceManager, job: &'a HeapJob<'a>) -> io::Result<HeapPlan<'a>> {
    let vs = space.volumes();
    let hv = job.hfid.vpid();
    let hp = vs.read_page(hv)?;
    let rec0 = SlottedPage::new(db_area(&hp))
        .and_then(|sp| sp.record(0))
        .ok_or_else(|| invalid(format!("heap header {hv:?} has no slot 0")))?
        .1;
    let hdr = HeapHdrStats::read(rec0, job.source_line)
        .map_err(|e| invalid(format!("heap header {hv:?}: {e}")))?;
    let bestspace: &[Vpid] = match &hdr.line {
        LineFields::Develop(d) => &d.pages,
        LineFields::V11_4(_) => &[],
    };
    let reuse_slots = FileHeader::read(&vs.read_page(job.hfid.vfid.header_vpid())?).file_type()
        == Some(FileType::HeapReuseSlots);
    let sectors = FileTable::read(vs, job.hfid.vfid)?
        .user_pages_by_sector()
        .into_iter()
        .map(|(_, pages)| {
            pages
                .into_iter()
                // the chain's last page is linked to an appended page, and so read, before its turn
                .filter(|v| *v != hv && *v != hdr.last_vpid && !bestspace.contains(v))
                .collect()
        })
        .collect();
    Ok(HeapPlan {
        job,
        class_oid: hdr.class_oid,
        reuse_slots,
        sectors,
    })
}

/// Rewrites page `vpid` of heap `h` if it is isolated; `Ok(None)` leaves it unwritten for the chain walk.
fn rewrite_isolated(
    space: &SpaceManager,
    horizon: &VacuumHorizon,
    h: &HeapPlan,
    vpid: Vpid,
) -> io::Result<Option<PageCounts>> {
    let mut st = HeapState::isolated(space, h.job.hfid, h.class_oid, h.reuse_slots);
    st.load(vpid)?;
    let chain = st.peek(vpid, |p| {
        let isolated = SlottedPage::new(db_area(p)).is_some_and(|sp| {
            (1..sp.num_slots()).all(|s| {
                !matches!(
                    sp.record(s).map(|(t, _)| t),
                    Some(RecordType::Relocation | RecordType::BigOne | RecordType::NewHome)
                )
            })
        });
        // a page outside the heap's chain is never written here; the chain walk checks the pages written are in it
        Ok(chain_of(p, vpid)
            .ok()
            .filter(|c| isolated && c.class_oid == h.class_oid))
    })?;
    let Some(chain) = chain else {
        return Ok(None);
    };
    let treatment = match h.job.re_encode {
        Some(re) => PageTreatment::Transform(re),
        None if horizon.page_needs_patch(&chain) => PageTreatment::Patch,
        None => return Ok(None),
    };
    match rewrite_current_page(&mut st, treatment) {
        Ok(c) => {
            st.flush()?;
            Ok(Some(c))
        }
        // dropped unflushed: the page stays as the source wrote it
        Err(e) if is_left_to_chain_walk(&e) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Rewrites the isolated pages of every heap of `jobs` that the chain walk would walk, on `workers` threads; returns
/// what it wrote ([`IsolatedPages::heap`] by index into `jobs`; nothing for a heap not walked).
pub fn rewrite_isolated_pages(
    space: &SpaceManager,
    horizon: &VacuumHorizon,
    jobs: &[HeapJob],
    workers: usize,
) -> io::Result<IsolatedPages> {
    let walked = |j: &HeapJob| j.re_encode.is_some() || horizon.pending;
    let plans: Vec<Option<HeapPlan>> = jobs
        .iter()
        .map(|j| walked(j).then(|| heap_plan(space, j)).transpose())
        .collect::<io::Result<_>>()?;
    let mut by_size: Vec<usize> = (0..jobs.len()).filter(|&i| plans[i].is_some()).collect();
    by_size.sort_by_key(|&i| std::cmp::Reverse(plans[i].as_ref().map_or(0, HeapPlan::pages)));
    let units: Vec<(usize, usize, usize)> = by_size
        .iter()
        .flat_map(|&i| {
            let n = plans[i].as_ref().map_or(0, |p| p.sectors.len());
            (0..n)
                .step_by(UNIT_SECTORS)
                .map(move |s| (i, s, (s + UNIT_SECTORS).min(n)))
        })
        .collect();
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let first_err: Mutex<Option<io::Error>> = Mutex::new(None);
    let set = IsolatedPages::new(space, jobs.len());
    let per_heap: Mutex<Vec<(u64, PageCounts)>> = Mutex::new(set.heaps.clone());
    std::thread::scope(|s| {
        for _ in 0..workers.clamp(1, units.len().max(1)) {
            s.spawn(|| {
                // worker-local results, merged once (PAR-17)
                let mut mine: Vec<(usize, u64, PageCounts)> = Vec::new();
                let r = (|| -> io::Result<()> {
                    loop {
                        let u = next.fetch_add(1, Ordering::Relaxed);
                        if u >= units.len() || failed.load(Ordering::Relaxed) {
                            return Ok(());
                        }
                        let (i, from, to) = units[u];
                        let Some(h) = plans[i].as_ref() else { continue };
                        if mine.last().is_none_or(|(j, _, _)| *j != i) {
                            mine.push((i, 0, PageCounts::default()));
                        }
                        let (_, n, acc) = mine.last_mut().unwrap();
                        for &vpid in h.sectors[from..to].iter().flatten() {
                            if let Some(c) = rewrite_isolated(space, horizon, h, vpid)? {
                                set.insert(vpid);
                                *n += 1;
                                acc.add(c);
                                if let Some(f) = h.job.pages_walked {
                                    f(1);
                                }
                            }
                        }
                    }
                })();
                match r {
                    Ok(()) => {
                        let mut o = per_heap.lock().unwrap();
                        for (i, n, c) in mine {
                            o[i].0 += n;
                            o[i].1.add(c);
                        }
                    }
                    Err(e) => {
                        failed.store(true, Ordering::Relaxed);
                        first_err.lock().unwrap().get_or_insert(e);
                    }
                }
            });
        }
    });
    match first_err.into_inner().unwrap() {
        Some(e) => Err(e),
        None => Ok(IsolatedPages {
            heaps: per_heap.into_inner().unwrap(),
            ..set
        }),
    }
}
