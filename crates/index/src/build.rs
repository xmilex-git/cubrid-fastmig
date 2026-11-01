//! Builds one index from its rows' (key, OID) runs: sort runs, split into shards, build the shards' leaves at
//! the same time, join them at the seams, build the non-leaf levels and the root.
//!
//! Input: the index's runs (in memory and spilled, from the heap scan's `RunWriter`s), the NULL-key row count,
//! the index shape and key order, the root and the B-tree file's sink (`btree_file` allocates the root with the file), the
//! shared worker limit, the spill directory and the merge memory of one worker (`intermediate_merge`).
//! Output: the root VPID, the root header written, the overflow key file if any, and `BtStats`.
//! Source: R6 § 2 steps 5–11 (engine no-logging parallel load, `btsort_px_*`, `bt_load_patch_seam`
//! btree_load.c:5481-5555, develop 5b1867174); workspace#481 G1 (one `--thread-count` limit for everything).

use crate::bt_stats::BtStats;
use crate::build_error::BuildError;
use crate::index_shape::IndexShape;
use crate::intermediate_merge::{MERGE_BUFFER, bound_open_runs};
use crate::key_order::KeyOrder;
use crate::kway_merge::{Cursor, KwayMerge};
use crate::leaf_builder::{ShardLeaves, build_shard};
use crate::nonleaf_builder::{NonleafLevels, build_nonleaf_levels, write_empty_root};
use crate::ovf_key_file::{MakeSink, OvfKeyFile};
use crate::page_sink::PageSink;
use crate::run_writer::IndexRuns;
use crate::sort_run::Run;
use crate::spill_dir::SpillDir;
use crate::spill_run::SpillCursor;
use crate::splitter::{select_splitters, slice_runs};
use crate::worker_limit::WorkerLimit;
use fastmig_ondisk::btree::node_header::NodeHeader;
use fastmig_ondisk::btree::root_header::RootHeader;
use fastmig_ondisk::{Vfid, Vpid};
use fastmig_progress::Meter;
use std::cmp::Ordering;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering as AtOrd};
use std::thread::Scope;
use std::time::{Duration, Instant};

/// Entries per extra shard: below this, splitting the leaves costs more than it saves.
pub const MIN_ENTRIES_PER_SHARD: usize = 4096;

pub struct BuiltIndex {
    pub root: Vpid,
    pub ovfid: Option<Vfid>,
    pub root_header: RootHeader,
    pub stats: BtStats,
    pub oids: u64,
    pub nulls: u64,
    pub timing: PhaseTimes,
}

#[derive(Clone, Debug, Default)]
pub struct PhaseTimes {
    pub sort: Duration,
    pub leaves: Duration,
    pub upper: Duration,
    pub workers: usize,
}

/// Runs `job(i)` for `i` in `0..n` on the calling thread, which holds the build's own permit, and on one more thread
/// per permit it borrows. A free permit is checked for before every job, so a build that began while every permit was
/// taken spreads over the permits other builds give back as they end. Without that, the largest index, started first
/// (LPT) when all builds hold a permit, built its leaves in one shard at about 1.6 M keys/s long after the others
/// had ended (workspace#609). Returns the threads it ran on.
fn parallel_for<E: Send>(
    limit: &WorkerLimit,
    n: usize,
    job: impl Fn(usize) -> Result<(), E> + Sync,
) -> Result<usize, E> {
    let pool = BorrowingPool {
        limit,
        n,
        next: AtomicUsize::new(0),
        threads: AtomicUsize::new(1),
        first_err: Mutex::new(None),
        job,
    };
    std::thread::scope(|s| pool.work(s));
    let threads = pool.threads.into_inner();
    pool.first_err
        .into_inner()
        .unwrap()
        .map_or(Ok(threads), Err)
}

/// The shared state of one `parallel_for`.
struct BorrowingPool<'a, E, F> {
    limit: &'a WorkerLimit,
    n: usize,
    next: AtomicUsize,
    threads: AtomicUsize,
    first_err: Mutex<Option<E>>,
    job: F,
}

impl<E: Send, F: Fn(usize) -> Result<(), E> + Sync> BorrowingPool<'_, E, F> {
    fn work<'s>(&'s self, s: &'s Scope<'s, '_>) {
        loop {
            let i = self.next.fetch_add(1, AtOrd::Relaxed);
            if i >= self.n || self.first_err.lock().unwrap().is_some() {
                break;
            }
            // more jobs left than threads running: borrow a free permit for one more thread
            if self.n - i > self.threads.load(AtOrd::Relaxed) {
                let permit = self.limit.try_acquire(1);
                if permit.count == 1 {
                    self.threads.fetch_add(1, AtOrd::Relaxed);
                    s.spawn(move || {
                        let _permit = permit;
                        self.work(s);
                    });
                }
            }
            if let Err(e) = (self.job)(i) {
                self.first_err.lock().unwrap().get_or_insert(e);
            }
        }
    }
}

/// One index to build: its root, shape, key order, runs, NULL-key rows, and where its pages go.
pub struct IndexJob<'a, 'm> {
    /// The root page, allocated with the file before the build (`btree_file::create_btree_file`).
    pub root: Vpid,
    pub shape: IndexShape,
    pub order: &'a dyn KeyOrder,
    pub runs: IndexRuns<'m>,
    pub nulls: u64,
    pub sink: &'a dyn PageSink,
    pub make_ovf_key_sink: &'a dyn MakeSink,
    /// Counts the pairs written to leaves (the index-build phase's second half, workspace#560).
    pub progress: Option<Meter<'a>>,
}

pub fn build_index(
    job: IndexJob,
    worker_limit: &WorkerLimit,
    spill: &SpillDir,
    merge_budget: usize,
) -> Result<BuiltIndex, BuildError> {
    let IndexJob {
        root,
        shape,
        order,
        mut runs,
        nulls,
        sink,
        make_ovf_key_sink,
        progress,
    } = job;
    let shape = &shape;
    let _own = worker_limit.acquire(1);
    let ovf_keys = OvfKeyFile::new(make_ovf_key_sink);

    let t0 = Instant::now();
    // no more spilled runs than one merge holds buffers for in the merge memory of a worker; every shard below
    // holds a worker permit, so the shards running at once stay within the merge memory of the workers
    bound_open_runs(&mut runs, order, spill, merge_budget, None)?;
    // spilled runs are sorted already; the in-memory ones sort inside the grant they were collected under
    let workers = {
        let slots: Vec<Mutex<&mut Run>> = runs.mem.iter_mut().map(Mutex::new).collect();
        parallel_for::<()>(worker_limit, slots.len(), |i| {
            slots[i].lock().unwrap().sort(order);
            Ok(())
        })
        .unwrap()
    };
    let t_sort = t0.elapsed();

    let t1 = Instant::now();
    // as many shards as the whole limit runs, whatever is free now: threads join as other builds give permits back
    let shard_count = (runs.entries() / MIN_ENTRIES_PER_SHARD).clamp(1, worker_limit.limit());
    let splitters = select_splitters(&runs, shard_count, order);
    let bounds = slice_runs(&runs, &splitters, order)?;
    let shard_total = splitters.len() + 1;
    let shards: Vec<Mutex<Option<ShardLeaves>>> =
        (0..shard_total).map(|_| Mutex::new(None)).collect();
    parallel_for(worker_limit, shard_total, |s| {
        let mut cursors = Vec::with_capacity(runs.mem.len() + runs.spilled.len());
        for (run, b) in runs.mem.iter().zip(&bounds.mem) {
            cursors.push(Cursor::Mem {
                run,
                at: b[s],
                end: b[s + 1],
            });
        }
        for (run, b) in runs.spilled.iter().zip(&bounds.spilled) {
            cursors.push(Cursor::File(SpillCursor::open(
                run,
                b[s],
                b[s + 1].index,
                MERGE_BUFFER,
            )?));
        }
        let mut stream = KwayMerge::new(cursors, order);
        let built = build_shard(shape, order, sink, &ovf_keys, &mut stream, progress)?;
        *shards[s].lock().unwrap() = Some(built);
        Ok::<(), BuildError>(())
    })?;
    let shards: Vec<ShardLeaves> = shards
        .into_iter()
        .filter_map(|m| m.into_inner().unwrap())
        .filter(|s| !s.leaves.is_empty())
        .collect();

    // seams: link each shard's last leaf to the next shard's first (bt_load_patch_seam), and take back the
    // partial-key counts a shard's first key counted as new although it shares columns with the previous shard
    let mut stats = BtStats {
        pkeys: vec![0; order.columns()],
        ..Default::default()
    };
    let mut oids = 0;
    let mut leaves = Vec::new();
    let mut held: Vec<crate::leaf_builder::LeafPage> = Vec::new();
    let mut prev_last_key: Option<Vec<u8>> = None;
    for mut sh in shards {
        if let Some(prev) = &prev_last_key {
            let first = &sh.leaves[0].first_key;
            assert_eq!(
                order.compare(prev, first),
                Ordering::Less,
                "shards overlap at a seam"
            );
            for n in &mut stats.pkeys[..order.common_columns(prev, first)] {
                *n -= 1;
            }
        }
        let first = sh.first.as_mut().unwrap();
        if let Some(left) = held.last_mut() {
            left.header.next = first.page.vpid;
            first.header.prev = left.page.vpid;
        }
        held.extend(sh.first.take());
        held.extend(sh.last.take());
        let c = &sh.counts;
        stats.keys += c.keys;
        oids += c.oids;
        stats.leafs += c.leaf_pages + c.ovf_oid_pages;
        for (a, b) in stats.pkeys.iter_mut().zip(&c.pkeys) {
            *a += b;
        }
        prev_last_key = Some(sh.last_key);
        leaves.extend(sh.leaves);
    }
    // the first key's leading NULL columns are no new prefix to the engine (`KeyOrder::leading_null_columns`)
    if let Some(first) = leaves.first() {
        for n in &mut stats.pkeys[..order.leading_null_columns(&first.first_key)] {
            *n -= 1;
        }
    }
    for p in held {
        p.write(sink)?;
    }
    // the keys are in the leaves now: give back their memory and remove the spill files
    drop(runs);
    let t_leaves = t1.elapsed();

    let t2 = Instant::now();
    let unique = shape.unique;
    let stat = |n: u64| if unique { n as i64 } else { -1 };
    let mut root_header = RootHeader {
        node: NodeHeader::new(1),
        num_oids: stat(oids + nulls),
        num_nulls: stat(nulls),
        num_keys: stat(stats.keys),
        topclass_oid: shape.topclass,
        unique_pk: shape.unique_pk(),
        off68: shape.root_off68,
        ovfid: Vfid::NULL,
        creator_mvccid: 0,
        packed_key_domain: shape.packed_key_domain.clone(),
    };
    let NonleafLevels {
        height,
        pages: upper_pages,
    } = if leaves.is_empty() {
        write_empty_root(sink, root, &mut root_header, shape.db_pagesize())?;
        NonleafLevels {
            height: 1,
            pages: 1,
        }
    } else {
        build_nonleaf_levels(shape, sink, &ovf_keys, root, leaves, &mut root_header)?
    };
    stats.height = height as u64;
    stats.pages = stats.leafs + upper_pages;
    Ok(BuiltIndex {
        root,
        ovfid: ovf_keys.vfid(),
        root_header,
        stats,
        oids,
        nulls,
        timing: PhaseTimes {
            sort: t_sort,
            leaves: t_leaves,
            upper: t2.elapsed(),
            workers,
        },
    })
}
