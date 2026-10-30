//! The default throughput of each kind of work: the first estimate of a run's total time before any phase has
//! been measured. Every default sits here, in one table, so a better measurement replaces one number.
//!
//! Input: none. Output: [`DefaultRate`] constants. The monitor scales them by the run's measured speed
//! ([`crate::estimate`]), so a default only has to be the right order of magnitude.
//! A rate is a list of measured points (worker limit, units/s of the whole phase), not one per-worker rate times
//! the workers: page treatment, index build and verify do not scale linearly (workspace#560).
//! Source: dev-host measurements cited per constant; a constant without a measurement says so.

use std::time::Duration;

/// Measured units per second of a whole phase at some worker limits, ascending by workers.
#[derive(Clone, Copy, Debug)]
pub struct DefaultRate {
    pub points: &'static [(usize, f64)],
}

impl DefaultRate {
    /// The rate at `workers`: linear between the two measured points around it, proportional below the first
    /// point, the last point's rate past it (no measurement says more workers help).
    pub fn with_workers(self, workers: usize) -> f64 {
        let w = workers.max(1) as f64;
        let Some(&(w0, r0)) = self.points.first() else {
            return 0.0;
        };
        if w <= w0 as f64 {
            return r0 * w / w0 as f64;
        }
        for pair in self.points.windows(2) {
            let ((wa, ra), (wb, rb)) = (pair[0], pair[1]);
            if w <= wb as f64 {
                let (wa, wb) = (wa as f64, wb as f64);
                return ra + (rb - ra) * (w - wa) / (wb - wa);
            }
        }
        self.points.last().map_or(0.0, |p| p.1)
    }
}

/// Byte copy of a volume: 1 GiB in 0.35 s with 256 KiB blocks, page cache warm (dd, workspace#494). One stream,
/// not scaled by workers: the device bounds it.
pub const COPY_BYTES: DefaultRate = DefaultRate {
    points: &[(1, (1u64 << 30) as f64 / 0.35)],
};

/// fsync of the files the run created: 4.5 GB in 0.66..1.56 s (clone / byte copy) on the large 11.4 DB, dev host
/// NVMe (workspace#560 report.json survey).
pub const FSYNC_BYTES: DefaultRate = DefaultRate {
    points: &[(1, 3.0e9)],
};

/// The rewritten ranges of the clones taken by writing the source bytes over them, then an fdatasync per volume
/// (`take_cloned_ranges`, workspace#556): 192 MiB in 0.11 s as a byte copy with fsync (workspace#556 commit). Bytes
/// written; the zeroed ranges are one fallocate each and not counted.
pub const CLONED_RANGE_BYTES: DefaultRate = DefaultRate {
    points: &[(1, 1.7e9)],
};

/// Index build keys, large 11.4 DB (73.0 M keys, 77 indexes): 72.3 / 20.0 / 10.0 s at 1 / 4 / 16 workers
/// (workspace#536 runs, report.json survey of workspace#560).
pub const INDEX_KEYS: DefaultRate = DefaultRate {
    points: &[(1, 1.01e6), (4, 3.65e6), (16, 7.3e6)],
};

/// Heap pages walked by the page treatment (patch or re-encode), large 11.4 DB (75.9 k pages): 23.1 / 6.5 / 2.4 s at
/// 1 / 4 / 16 workers (workspace#536 runs, report.json survey of workspace#560).
pub const HEAP_PAGES: DefaultRate = DefaultRate {
    points: &[(1, 3.3e3), (4, 11.6e3), (16, 31.0e3)],
};

/// Index pairs the verifier merges, target and source together: 146 M pairs in 38.5 s at 16 workers on the large
/// 11.4 DB (workspace#553 run, one batch per side would be faster). Below 16 workers not yet measured: scaled down
/// like the index build.
pub const VERIFY_PAIRS: DefaultRate = DefaultRate {
    points: &[(1, 0.53e6), (4, 1.9e6), (16, 3.8e6)],
};

/// A short fixed step (open, write a header, register): 0..25 ms each on T1 and the large DBs (workspace#560).
pub const STEP: Duration = Duration::from_millis(10);
