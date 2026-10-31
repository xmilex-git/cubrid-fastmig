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

/// Heap pages the index build's scan reads (its first part): the large 11.4 DB (111.4 k pages) in about 43.5 / 10.1 /
/// 2.2 s and the large develop DB (105.0 k pages) in 44.9 / 10.9 / 2.9 s at 1 / 4 / 16 workers, read from the 2 s
/// progress lines of workspace#560 pass 2 (CPUs 16-31); the rate is the mean of the two. Pages, not keys: a page
/// costs its read and its records whatever keys its class has (keys per page run 289..973 between classes there).
pub const INDEX_SCAN_PAGES: DefaultRate = DefaultRate {
    points: &[(1, 2.45e3), (4, 10.35e3), (16, 43.5e3)],
};

/// Index build keys written to leaves (the index build's second part: merge, leaves, upper levels): 73.0 M keys in
/// the rest of the same runs, about 28.0 / 9.3 / 7.6 s at 1 / 4 / 16 workers.
pub const TREE_BUILD_KEYS: DefaultRate = DefaultRate {
    points: &[(1, 2.6e6), (4, 7.85e6), (16, 9.6e6)],
};

/// Heap pages walked by the page treatment (patch or re-encode), large 11.4 DB (75.9 k pages): 22.8 / 6.1 / 1.9 s at
/// 1 / 4 / 16 workers (workspace#560 calibration pass 1, CPUs 16-31).
pub const HEAP_PAGES: DefaultRate = DefaultRate {
    points: &[(1, 3.3e3), (4, 12.5e3), (16, 39.9e3)],
};

/// Heap pages the verifier reads (its first part: heap pass and run sort, target and source): twice the pages above
/// in 134.2 / 34.6 / 9.9 s (11.4) and 102.8 / 27.2 / 8.8 s (develop) at 1 / 4 / 16 workers (verify report
/// `phases_s`, workspace#560 pass 2); the rate is the mean of the two (each within 0.85..1.12x of it).
pub const VERIFY_HEAP_PAGES: DefaultRate = DefaultRate {
    points: &[(1, 1.85e3), (4, 7.08e3), (16, 23.2e3)],
};

/// Index pairs the verifier merges (its second part), target and source together: 146.0 M pairs in 48.7 / 14.0 / 6.7
/// s at 1 / 4 / 16 workers, the mean of the same runs.
pub const VERIFY_MERGE_PAIRS: DefaultRate = DefaultRate {
    points: &[(1, 3.0e6), (4, 10.4e6), (16, 21.8e6)],
};

/// Pairs the precheck's counting scan (`resource_estimate`, `fastmig_index::pair_count`) counts on a source of the
/// target's release: 73.0 M in 26.5 / 6.6 / 1.78 s at 1 / 4 / 16 workers on the large develop DB (workspace#560
/// calibration pass 1, CPUs 16-31). It reads the heaps and extracts the keys as the index build and the verifier do,
/// so its time is the run's first CPU observation (workspace#560).
pub const PAIR_COUNT: DefaultRate = DefaultRate {
    points: &[(1, 2.75e6), (4, 11.1e6), (16, 41.1e6)],
};

/// The same counting scan on a source of an older line, each row of a re-encoded class re-encoded first: 73.0 M in
/// 43.1 / 10.9 / 2.97 s at 1 / 4 / 16 workers on the large 11.4 DB (workspace#560 calibration pass 1, pair count less
/// the appended-pages bound). Chosen when the source line is not the target's; whether the gap to [`PAIR_COUNT`]
/// comes from the re-encoding or from reading an older line's rows is not measured apart (that DB has both).
pub const PAIR_COUNT_RE_ENCODED: DefaultRate = DefaultRate {
    points: &[(1, 1.70e6), (4, 6.73e6), (16, 24.6e6)],
};

/// A short fixed step (open, write a header, register): 0..25 ms each on T1 and the large DBs (workspace#560).
pub const STEP: Duration = Duration::from_millis(10);
