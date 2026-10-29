//! Splits one index's check-2 merge into shards — ranges of its leaf list with the matching key ranges of its heap
//! runs — so a large index walks its leaves on several workers instead of one.
//!
//! Input: the opened tree (its leaf list with each leaf's separator), the index's sorted heap runs, the shard count.
//! Output: the leaf range of each shard and where each run's slice for it starts ([`ShardBounds`]).
//! A shard starts at a leaf whose parent record carries a separator `sep` (last key of the left leaf < sep ≤ first
//! key); heap pairs with a key below `sep` go left, the rest right (`fastmig_index::splitter::slice_runs`, the index
//! builder's shard cut), so on a well-formed tree each shard's leaves and heap pairs hold the same keys. A key's
//! record never spans two leaves, so no key is split. The checks across a cut are made by
//! `btree_walk::join_shards`. Limit: on a tree whose `separator` check already fails at a cut (a key on the wrong
//! side of its separator), that key's pairs are also reported missing on both sides.
//! Source: workspace#509 (leaf walk of one index was one thread: 10.8 M pairs in 5 s on the big develop DB).

use std::io;
use std::ops::Range;

use fastmig_index::IndexRuns;
use fastmig_index::splitter::{ShardBounds, slice_runs};

use crate::btree_walk::IndexTree;

/// Leaves per shard at least: below this, a shard's setup (spill files opened again, a join) is not worth it.
pub const MIN_SHARD_LEAVES: usize = 1024;

/// The shard count for a tree of `leaves` leaves on `workers` workers.
pub fn shard_count(leaves: usize, workers: usize) -> usize {
    (leaves / MIN_SHARD_LEAVES).clamp(1, workers.max(1))
}

/// Shard cuts of `tree`: leaf positions near even splits whose separators strictly increase, so key ranges never
/// overlap. Empty when `shards` < 2 or no leaf after the first carries a separator.
pub fn leaf_cuts(tree: &IndexTree, shards: usize) -> Vec<usize> {
    let n = tree.leaves.len();
    let mut cuts: Vec<usize> = Vec::new();
    for s in 1..shards {
        // the first leaf at or after the even cut that carries a separator, past the previous cut
        let from = (s * n / shards).max(cuts.last().map_or(1, |c| c + 1));
        let Some(c) = (from..n).find(|&i| tree.leaves[i].1.is_some()) else {
            break;
        };
        // a damaged tree's out-of-order separator is no cut
        if cuts.last().is_none_or(|&p| {
            tree.kd
                .compare(tree.separator(p), tree.separator(c))
                .is_lt()
        }) {
            cuts.push(c);
        }
    }
    cuts
}

/// The leaf ranges `cuts` make, in leaf order, covering every leaf.
fn ranges(cuts: &[usize], leaves: usize) -> Vec<Range<usize>> {
    let starts = std::iter::once(0).chain(cuts.iter().copied());
    let ends = cuts.iter().copied().chain(std::iter::once(leaves));
    starts.zip(ends).map(|(a, b)| a..b).collect()
}

/// Leaf ranges and the run slices of each (`None`: one range).
pub fn plan_shards(
    tree: &IndexTree,
    runs: &IndexRuns,
    shards: usize,
) -> io::Result<(Vec<Range<usize>>, Option<ShardBounds>)> {
    let cuts = leaf_cuts(tree, shards);
    let bounds = if cuts.is_empty() {
        None
    } else {
        let splitters: Vec<Vec<u8>> = cuts.iter().map(|&c| tree.separator(c).to_vec()).collect();
        Some(slice_runs(runs, &splitters, &tree.kd)?)
    };
    Ok((ranges(&cuts, tree.leaves.len()), bounds))
}
