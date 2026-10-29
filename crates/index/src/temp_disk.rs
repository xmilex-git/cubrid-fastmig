//! The temp disk limit (`--temp-disk`): the bytes spill files may hold at once, kept by scheduling, not by failing.
//!
//! Input: per index the bytes its (key, OID) pairs take as spill records (`pair_count::pair_bytes`, counted on the
//! target before the build, or on a develop source for the plan) and the limit.
//! Output: [`index_batches`] — the indexes cut into batches whose pair bytes fit the limit (each batch is one
//! heap scan of its owner classes, then its builds, then its spill files go); [`TEMP_DISK`] — the spill bytes on
//! disk now and at most, for the progress display and report.json.
//! Spill bound of one scan: a spill record is `key_len u32 · OID 8 B · key` (`spill_run`), so the pairs of a batch
//! take `T` bytes as records, and a pair is spilled at most once (the build after the scan spills nothing more):
//! a scan spills at most `T`. Memory gives no credit: a writer spills when its own part of its worker's share is
//! full, whatever room the limit has (T1 develop dirty at `--mem` 530 MiB spilled 832 KiB where `T − (mem/2 −
//! share)` gave 0).
//! Source: workspace#526 (user request: no disk full during a run, the size chosen before it starts).

use std::sync::atomic::{AtomicU64, Ordering};

use fastmig_ondisk::Oid;

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

/// Cuts indexes (their pair bytes, by position) into batches whose pair bytes fit `limit`, largest first, first
/// fit. Err: the smallest limit that works (the largest single index) when `limit` is below it.
pub fn index_batches(pair_bytes: &[u64], limit: u64) -> Result<Vec<Vec<usize>>, u64> {
    let min = pair_bytes.iter().copied().max().unwrap_or(0);
    if min > limit {
        return Err(min);
    }
    let mut order: Vec<usize> = (0..pair_bytes.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(pair_bytes[i]));
    let mut batches: Vec<(u64, Vec<usize>)> = Vec::new();
    for i in order {
        let t = pair_bytes[i];
        match batches.iter_mut().find(|(sum, _)| sum + t <= limit) {
            Some((sum, b)) => {
                *sum += t;
                b.push(i);
            }
            None => batches.push((t, vec![i])),
        }
    }
    Ok(batches.into_iter().map(|(_, b)| b).collect())
}
