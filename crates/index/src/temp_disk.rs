//! The temp disk limit (`--temp-disk`): the bytes spill files may hold at once, kept by scheduling, not by failing.
//!
//! Input: per index the bytes its (key, OID) pairs take as spill records (`IndexPairBytes`, from the plan's
//! estimate scan), the limit, the memory limit and the worker count. Output: [`index_batches`] — the indexes cut
//! into batches whose spill bound fits the limit (each batch is one heap scan of its owner classes, then its
//! builds, then its spill files go); [`TEMP_DISK`] — the spill bytes on disk now and at most, for the progress
//! display and report.json.
//! Spill bound of one scan ([`spill_bound`]): a spill record is `key_len u32 · OID 8 B · key` (`spill_run`), so
//! the pairs of a batch take `T` bytes as records. The heap scan gives each worker a share of at most
//! `min(WORKER_SHARE, mem/2)` and keeps a finished run in memory while the limit has room for it, so at least
//! `mem/2 − share` bytes of finished runs stay in memory before anything spills; at most `T − (mem/2 − share)` is
//! spilled. The build after the scan spills nothing more.
//! Source: workspace#526 (user request: no disk full during a run, the size chosen before it starts).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use fastmig_ondisk::Oid;

use crate::heap_scan::WORKER_SHARE;

/// Spill bytes on disk: now and the most at any moment of the run (Relaxed: a display, not a guard).
pub struct TempDiskUsage {
    now: AtomicU64,
    peak: AtomicU64,
}

pub static TEMP_DISK: TempDiskUsage = TempDiskUsage {
    now: AtomicU64::new(0),
    peak: AtomicU64::new(0),
};

impl TempDiskUsage {
    pub fn add(&self, bytes: u64) {
        let now = self.now.fetch_add(bytes, Ordering::Relaxed) + bytes;
        self.peak.fetch_max(now, Ordering::Relaxed);
    }

    pub fn sub(&self, bytes: u64) {
        self.now.fetch_sub(bytes, Ordering::Relaxed);
    }

    pub fn now_and_peak(&self) -> (u64, u64) {
        (
            self.now.load(Ordering::Relaxed),
            self.peak.load(Ordering::Relaxed),
        )
    }
}

/// Bytes one spill record of a pair with a `key_len`-byte key takes (`spill_run` record header + key).
pub fn spill_record_bytes(key_len: u64) -> u64 {
    4 + Oid::SIZE as u64 + key_len
}

/// The most one heap scan spills when its pairs take `pair_bytes` as spill records, under `memory_limit` bytes.
pub fn spill_bound(pair_bytes: u64, memory_limit: u64) -> u64 {
    let share = (WORKER_SHARE as u64).min(memory_limit / 2);
    pair_bytes.saturating_sub((memory_limit / 2).saturating_sub(share))
}

/// Cuts indexes (their pair bytes, by position) into batches whose [`spill_bound`] fits `limit`, largest first,
/// first fit. Err: the smallest limit that works (the largest single index's bound) when `limit` is below it.
pub fn index_batches(
    pair_bytes: &[u64],
    limit: u64,
    memory_limit: u64,
) -> Result<Vec<Vec<usize>>, u64> {
    let min = pair_bytes
        .iter()
        .map(|&t| spill_bound(t, memory_limit))
        .max()
        .unwrap_or(0);
    if min > limit {
        return Err(min);
    }
    let mut order: Vec<usize> = (0..pair_bytes.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(pair_bytes[i]));
    let mut batches: Vec<(u64, Vec<usize>)> = Vec::new();
    for i in order {
        let t = pair_bytes[i];
        match batches
            .iter_mut()
            .find(|(sum, _)| spill_bound(sum + t, memory_limit) <= limit)
        {
            Some((sum, b)) => {
                *sum += t;
                b.push(i);
            }
            None => batches.push((t, vec![i])),
        }
    }
    Ok(batches.into_iter().map(|(_, b)| b).collect())
}

/// What a run with `--temp-disk` gives the index rebuild: the limit and, per (class OID, constraint name), the
/// bytes that index's pairs take as spill records (the plan's estimate scan, an upper bound).
pub struct TempDiskPlan {
    pub limit: u64,
    pub pair_bytes: BTreeMap<(Oid, Vec<u8>), u64>,
}
