//! Splits the sorted runs into key ranges, one per shard, so shards build their leaves at the same time.
//!
//! Input: sorted runs and the shard count. Output: per shard, the slice of each run that falls in its range.
//! Source: engine `btsort_px_select_splitters` (weighted quantiles over run samples, distinct keys only, strictly
//! increasing) and `btsort_px_slice_runs_index_leaf` (key < / ≥ splitter), btree_sort.c develop 5b1867174;
//! R6 § 4.2 R-SORT-3: a key's OIDs never span two shards, because a range starts at a key's first entry.

use crate::key_order::KeyOrder;
use crate::sort_run::Run;
use std::cmp::Ordering;
use std::ops::Range;

/// Samples taken from each run.
const SAMPLES_PER_RUN: usize = 256;

/// Splitter keys: at most `shards - 1`, strictly increasing.
pub fn select_splitters(runs: &[Run], shards: usize, order: &dyn KeyOrder) -> Vec<Vec<u8>> {
    if shards < 2 {
        return Vec::new();
    }
    let mut samples: Vec<(&[u8], f64)> = Vec::new();
    for run in runs.iter().filter(|r| !r.is_empty()) {
        let n = run.len().min(SAMPLES_PER_RUN);
        let weight = run.len() as f64 / n as f64;
        for i in 0..n {
            samples.push((run.key(i * run.len() / n), weight));
        }
    }
    samples.sort_by(|a, b| order.compare(a.0, b.0));
    let total: f64 = samples.iter().map(|s| s.1).sum();
    let mut out: Vec<Vec<u8>> = Vec::new();
    let mut acc = 0.0;
    let mut next = 1;
    for (key, w) in samples {
        acc += w;
        while next < shards && acc >= total * next as f64 / shards as f64 {
            next += 1;
            if out
                .last()
                .is_none_or(|last| order.compare(last, key) == Ordering::Less)
            {
                out.push(key.to_vec());
            }
        }
    }
    // the first key of all is no boundary: the shard before it would be empty
    out
}

/// First index of `run` whose key is ≥ `key`.
fn lower_bound(run: &Run, key: &[u8], order: &dyn KeyOrder) -> usize {
    let (mut lo, mut hi) = (0, run.len());
    while lo < hi {
        let mid = (lo + hi) / 2;
        if order.compare(run.key(mid), key) == Ordering::Less {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

/// `slices[s][r]`: the range of run `r` that shard `s` takes (`splitters.len() + 1` shards).
pub fn slice_runs(
    runs: &[Run],
    splitters: &[Vec<u8>],
    order: &dyn KeyOrder,
) -> Vec<Vec<Range<usize>>> {
    let bounds: Vec<Vec<usize>> = runs
        .iter()
        .map(|run| {
            let mut b = vec![0];
            b.extend(splitters.iter().map(|k| lower_bound(run, k, order)));
            b.push(run.len());
            b
        })
        .collect();
    (0..=splitters.len())
        .map(|s| bounds.iter().map(|b| b[s]..b[s + 1]).collect())
        .collect()
}
