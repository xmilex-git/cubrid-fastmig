//! Builds one index from its rows' (key, OID) runs: sort runs, split into shards, build the shards' leaves at
//! the same time, join them at the seams, build the non-leaf levels and the root.
//!
//! Input: one run per producer (filled by the heap scan), the NULL-key row count, the index shape and key
//! order, the B-tree file's sink (its first page is the root) and the shared worker budget.
//! Output: the root VPID, the root header written, the overflow key file if any, and `BtStats`.
//! Source: R6 § 2 steps 5–11 (engine no-logging parallel load, `btsort_px_*`, `bt_load_patch_seam`
//! btree_load.c:5481-5555, develop 5b1867174); workspace#481 G1 (one `--workers` limit for everything).

use crate::bt_stats::BtStats;
use crate::build_error::BuildError;
use crate::index_shape::IndexShape;
use crate::key_order::KeyOrder;
use crate::kway_merge::KwayMerge;
use crate::leaf_builder::{ShardLeaves, build_shard};
use crate::nonleaf_builder::{build_nonleaf_levels, write_empty_root};
use crate::ovf_key_file::{MakeSink, OvfKeyFile};
use crate::page_sink::PageSink;
use crate::sort_run::Run;
use crate::splitter::{select_splitters, slice_runs};
use crate::worker_budget::WorkerBudget;
use fastmig_ondisk::btree::node_header::NodeHeader;
use fastmig_ondisk::btree::root_header::RootHeader;
use fastmig_ondisk::{Vfid, Vpid};
use std::cmp::Ordering;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering as AtOrd};
use std::time::{Duration, Instant};

/// Entries per extra shard: below this, splitting the leaves costs more than it saves.
const MIN_ENTRIES_PER_SHARD: usize = 4096;

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

/// Runs `job(i)` for `i` in `0..n` on `threads` threads.
fn parallel_for<E: Send>(
    threads: usize,
    n: usize,
    job: impl Fn(usize) -> Result<(), E> + Sync,
) -> Result<(), E> {
    let next = AtomicUsize::new(0);
    let first_err = Mutex::new(None);
    std::thread::scope(|s| {
        for _ in 0..threads.min(n).max(1) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, AtOrd::Relaxed);
                    if i >= n || first_err.lock().unwrap().is_some() {
                        break;
                    }
                    if let Err(e) = job(i) {
                        first_err.lock().unwrap().get_or_insert(e);
                    }
                }
            });
        }
    });
    first_err.into_inner().unwrap().map_or(Ok(()), Err)
}

pub fn build_index(
    shape: &IndexShape,
    order: &dyn KeyOrder,
    mut runs: Vec<Run>,
    nulls: u64,
    sink: &dyn PageSink,
    make_ovf_key_sink: &MakeSink,
    budget: &WorkerBudget,
) -> Result<BuiltIndex, BuildError> {
    let _own = budget.acquire(1);
    let root = sink.alloc_page()?;
    let ovf_keys = OvfKeyFile::new(make_ovf_key_sink);

    let t0 = Instant::now();
    let extra = budget.try_acquire(runs.len().saturating_sub(1));
    let workers = 1 + extra.count;
    {
        let slots: Vec<Mutex<&mut Run>> = runs.iter_mut().map(Mutex::new).collect();
        parallel_for::<()>(workers, slots.len(), |i| {
            slots[i].lock().unwrap().sort(order);
            Ok(())
        })
        .unwrap();
    }
    drop(extra);
    let t_sort = t0.elapsed();

    let t1 = Instant::now();
    let entries: usize = runs.iter().map(Run::len).sum();
    let extra = budget.try_acquire((entries / MIN_ENTRIES_PER_SHARD).saturating_sub(1));
    let shard_count = 1 + extra.count;
    let splitters = select_splitters(&runs, shard_count, order);
    let slices = slice_runs(&runs, &splitters, order);
    let shards: Vec<Mutex<Option<ShardLeaves>>> = slices.iter().map(|_| Mutex::new(None)).collect();
    parallel_for(shard_count, slices.len(), |s| {
        let stream = KwayMerge::new(&runs, &slices[s], order);
        let built = build_shard(shape, order, sink, &ovf_keys, stream)?;
        *shards[s].lock().unwrap() = Some(built);
        Ok::<(), BuildError>(())
    })?;
    drop(extra);
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
    for p in held {
        p.write(sink)?;
    }
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
        off68: 0,
        ovfid: Vfid::NULL,
        creator_mvccid: 0,
        packed_key_domain: shape.packed_key_domain.clone(),
    };
    let (height, upper_pages) = if leaves.is_empty() {
        write_empty_root(sink, root, &mut root_header, shape.db_pagesize())?;
        (1, 1)
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
