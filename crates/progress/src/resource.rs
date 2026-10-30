//! The resource that bounds a phase: CPU work, or storage reads, or storage writes. A phase's bottleneck is not fixed:
//! the same index build is CPU-bound on NVMe and may wait on random reads or temp I/O on a disk. Each phase starts
//! with a prior (from its spec) and, once it has run long enough, is classified by what it used.
//!
//! Input: a phase's elapsed seconds, the worker limit, and its [`ProcSample`] use. Output: [`Resource`].
//! Rule (workspace#560, 1번 세션 decision 2026-10-30): CPU busy share = CPU seconds / (elapsed x workers); at or above
//! [`CPU_BUSY`] the phase is CPU-bound, below it bound by the storage side it moved more bytes on; a phase that moved
//! no device bytes is CPU-bound (nothing else to wait on that we can measure).

use crate::proc_sample::ProcSample;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resource {
    Cpu,
    Read,
    Write,
}

/// CPU busy share at or above which a phase counts as CPU-bound.
pub const CPU_BUSY: f64 = 0.5;

impl Resource {
    pub const ALL: [Resource; 3] = [Resource::Cpu, Resource::Read, Resource::Write];

    pub fn name(self) -> &'static str {
        match self {
            Resource::Cpu => "cpu",
            Resource::Read => "read",
            Resource::Write => "write",
        }
    }

    pub fn classify(used: ProcSample, elapsed_s: f64, workers: usize) -> Resource {
        let busy = used.cpu_s / (elapsed_s * workers.max(1) as f64).max(1e-9);
        if busy >= CPU_BUSY || used.read_bytes + used.write_bytes == 0 {
            Resource::Cpu
        } else if used.read_bytes >= used.write_bytes {
            Resource::Read
        } else {
            Resource::Write
        }
    }
}
