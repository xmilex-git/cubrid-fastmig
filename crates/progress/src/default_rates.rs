//! The default throughput of each kind of work: the first estimate of a run's total time before any phase has
//! been measured. Every default sits here, in one table, so a better measurement replaces one number.
//!
//! Input: none. Output: [`DefaultRate`] constants. The monitor scales them by the run's measured speed
//! ([`crate::estimate`]), so a default only has to be the right order of magnitude.
//! Source: dev-host measurements cited per constant; a constant without a measurement says so.

use std::time::Duration;

/// Units per second of one worker, and how many workers the work scales over.
#[derive(Clone, Copy, Debug)]
pub struct DefaultRate {
    pub per_worker: f64,
    /// Workers past this add nothing (the work is bound by one device, or serial).
    pub max_workers: usize,
}

impl DefaultRate {
    pub fn with_workers(self, workers: usize) -> f64 {
        self.per_worker * workers.clamp(1, self.max_workers.max(1)) as f64
    }
}

/// Byte copy of a volume: 1 GiB in 0.35 s with 256 KiB blocks, page cache warm (dd, workspace#494). One stream,
/// not scaled by workers: the device bounds it.
pub const COPY_BYTES: DefaultRate = DefaultRate {
    per_worker: (1u64 << 30) as f64 / 0.35,
    max_workers: 1,
};

/// fsync of freshly written bytes: 4.3 GiB in 2.76 s after a 4.2 GiB byte copy, dev host NVMe (migrate run,
/// workspace#472 progress task).
pub const FSYNC_BYTES: DefaultRate = DefaultRate {
    per_worker: 1.5 * (1u64 << 30) as f64,
    max_workers: 1,
};

/// Index build keys: 3 M (key, OID) pairs in 3.21 / 0.81 / 0.44 / 0.26 s with 1 / 4 / 8 / 16 workers
/// (P1d workspace#493).
pub const INDEX_KEYS: DefaultRate = DefaultRate {
    per_worker: 3.0e6 / 3.21,
    max_workers: 16,
};

/// Heap record re-encoding (P1b workspace#491). Not measured; decode+encode of a ~100 B record.
pub const REENCODE_RECORDS: DefaultRate = DefaultRate {
    per_worker: 1.0e6,
    max_workers: 64,
};

/// A short fixed step (open, write a header, register).
pub const STEP: Duration = Duration::from_millis(50);
