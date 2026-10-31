//! The bottleneck of a phase: CPU work, or storage reads, or storage writes. It is not fixed: the same index build
//! is CPU-bound on NVMe and may wait on random reads or temp I/O on a disk. Each phase starts with a prior (from its
//! spec) and, once it has run long enough, is classified by what it used.
//!
//! Input: a phase's elapsed seconds, the worker limit, its [`ProcSample`] use and the throughput of its devices.
//! Output: [`Bottleneck`], or `None` when the use shows none (the phase then keeps its prior and moves no speed
//! factor).
//! Rule (workspace#560, 1번 세션 decisions 2026-10-30/31): CPU busy share = CPU seconds / (elapsed x workers); at or
//! above [`BUSY`] the phase is CPU-bound. Otherwise it is bound by the storage side whose byte rate is the larger
//! share of that device's throughput, if that share is at least [`BUSY`]. A phase that neither kept the CPUs busy nor
//! moved bytes near a device's throughput (a clone or unshare on XFS, a wait) is not classified.

use crate::proc_sample::ProcSample;
use crate::storage_kind::StorageKind;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bottleneck {
    Cpu,
    Read,
    Write,
}

/// Share of the CPUs or of a device's throughput at or above which a phase counts as bound by it.
pub const BUSY: f64 = 0.5;

/// Bytes per second a phase's device reads and writes at its best: what its byte rate is compared with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DeviceThroughput {
    pub read: f64,
    pub write: f64,
}

impl DeviceThroughput {
    /// Dev host NVMe: fsync of 4.5 GB at 3.0 GB/s ([`crate::default_rates::FSYNC_BYTES`]); reads not measured apart,
    /// taken as the same. Per storage kind: [`DeviceThroughput::of`] (workspace#561).
    pub const NVME: DeviceThroughput = DeviceThroughput {
        read: 3.0e9,
        write: 3.0e9,
    };
    /// Dev host SATA SSD (Samsung MZ7L33T8) and HDD (Seagate ST12000NM000J): one-stream 1 MiB sequential read and
    /// write of `docs/storage_kinds.fio` (docs/resources.md § Storage kinds, workspace#561).
    pub const SATA_SSD: DeviceThroughput = DeviceThroughput {
        read: 0.546e9,
        write: 0.464e9,
    };
    pub const HDD: DeviceThroughput = DeviceThroughput {
        read: 0.231e9,
        write: 0.229e9,
    };

    /// The throughput a storage kind is taken to have; unknown takes the hdd values (docs/resources.md § Storage
    /// kinds).
    pub fn of(kind: StorageKind) -> DeviceThroughput {
        match kind {
            StorageKind::Nvme => DeviceThroughput::NVME,
            StorageKind::SataSsd => DeviceThroughput::SATA_SSD,
            StorageKind::Unknown | StorageKind::Hdd => DeviceThroughput::HDD,
        }
    }

    /// A phase that reads on the `read` kinds and writes on the `write` kinds: the slowest of each side.
    pub fn slowest(read: &[StorageKind], write: &[StorageKind]) -> DeviceThroughput {
        let min = |ks: &[StorageKind], side: fn(DeviceThroughput) -> f64| {
            ks.iter()
                .map(|&k| side(DeviceThroughput::of(k)))
                .fold(f64::INFINITY, f64::min)
        };
        DeviceThroughput {
            read: min(read, |d| d.read),
            write: min(write, |d| d.write),
        }
    }
}

impl Bottleneck {
    pub const ALL: [Bottleneck; 3] = [Bottleneck::Cpu, Bottleneck::Read, Bottleneck::Write];

    pub fn name(self) -> &'static str {
        match self {
            Bottleneck::Cpu => "cpu",
            Bottleneck::Read => "read",
            Bottleneck::Write => "write",
        }
    }

    pub fn classify(
        used: ProcSample,
        elapsed_s: f64,
        workers: usize,
        device: DeviceThroughput,
    ) -> Option<Bottleneck> {
        let s = elapsed_s.max(1e-9);
        let busy = used.cpu_s / (s * workers.max(1) as f64);
        let read = used.read_bytes as f64 / s / device.read;
        let write = used.write_bytes as f64 / s / device.write;
        if busy >= BUSY {
            Some(Bottleneck::Cpu)
        } else if read.max(write) < BUSY {
            None
        } else if read >= write {
            Some(Bottleneck::Read)
        } else {
            Some(Bottleneck::Write)
        }
    }
}
