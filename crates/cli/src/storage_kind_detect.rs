//! Finds the storage kind of the device a path lives on, from sysfs.
//!
//! Input: a path (it need not exist yet: its nearest existing ancestor is used). Output: [`Detected`], a
//! [`StorageKind`] and the block device names it was read from (for the plan).
//! A partition is judged by its whole disk; a device-mapper or md device by its slaves, the slowest of them.
//! A path without a block device behind it (`st_dev` major 0: btrfs, ZFS, overlay, tmpfs, NFS) or an unreadable sysfs
//! is [`StorageKind::Unknown`], shown as a failed detection; btrfs device lists are not followed (no btrfs host to
//! check it on, workspace#561).
//! Source: Linux `Documentation/ABI/stable/sysfs-block` (`/sys/dev/block/<major>:<minor>`, `partition`, `slaves/`,
//! `queue/rotational`); glibc `gnu_dev_major`/`gnu_dev_minor` for the dev_t split; workspace#561.
//! Rules and the per-phase devices: docs/resources.md § Storage kinds.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use fastmig_index::extent_jobs::ExtentReads;
use fastmig_progress::{
    DefaultRate, DeviceThroughput, PhaseSpec, StorageKind, Unit, default_rates,
};

use crate::write_probe::{self, WriteProbe};

/// The storage kind of one directory and the whole disks it was judged by (empty when sysfs had none).
#[derive(Clone, Debug)]
pub struct Detected {
    pub kind: StorageKind,
    pub disks: Vec<String>,
}

impl Detected {
    /// `hdd (sdc)`, `unknown (detection failed: no block device)`.
    pub fn describe(&self) -> String {
        let disks = if self.disks.is_empty() {
            "detection failed: no block device".to_string()
        } else {
            self.disks.join(", ")
        };
        format!("{} ({disks})", self.kind.name())
    }
}

/// The three places a run does I/O on, each judged on its own (workspace#561): the source volumes, the target
/// directory and the spill directory (`--spill-dir`, the temp side).
#[derive(Clone, Debug)]
pub struct RunStorage {
    pub source: Detected,
    pub target: Detected,
    pub temp: Detected,
    /// The target directory's write probe, taken by migrate when the target's kind is unknown (`None` in plan, for a
    /// known kind, or when the probe failed).
    pub target_write: Option<WriteProbe>,
}

impl RunStorage {
    /// The source is judged by every volume path (volumes may sit in several directories): the slowest of them.
    pub fn detect(source_volumes: &[PathBuf], target_dir: &Path, spill_dir: &Path) -> RunStorage {
        let (mut kind, mut disks) = (None::<StorageKind>, Vec::new());
        for d in source_volumes.iter().map(|v| storage_kind(v)) {
            kind = kind.max(Some(d.kind));
            for n in d.disks {
                if !disks.contains(&n) {
                    disks.push(n);
                }
            }
        }
        let source = Detected {
            kind: kind.unwrap_or(StorageKind::Unknown),
            disks,
        };
        RunStorage {
            source,
            target: storage_kind(target_dir),
            temp: storage_kind(spill_dir),
            target_write: None,
        }
    }

    /// The plan's line: `storage: source nvme (nvme0n1), target hdd (sdc), temp hdd (sdc)`.
    pub fn table(&self) -> String {
        format!(
            "storage: source {}, target {}, temp {}\n",
            self.source.describe(),
            self.target.describe(),
            self.temp.describe()
        )
    }

    /// Per phase of `specs`, by name, the throughput of the devices it reads and writes with `workers` workers: the
    /// `devices` of `Progress::new` (docs/resources.md § Storage kinds; a phase not listed there works on the target).
    pub fn phase_devices(&self, specs: &[PhaseSpec], workers: usize) -> Vec<DeviceThroughput> {
        let (s, t, m) = (self.source.kind, self.target.kind, self.temp.kind);
        specs
            .iter()
            .map(|p| match p.name {
                "precheck" | "open-sources" | "volume-copy" => {
                    DeviceThroughput::slowest(&[s], &[t], workers)
                }
                "index-build" => DeviceThroughput::slowest(&[t, m], &[t, m], workers),
                "verifier" => DeviceThroughput::slowest(&[s, t, m], &[m], workers),
                _ => DeviceThroughput::slowest(&[t], &[t], workers),
            })
            .collect()
    }

    /// `specs` with the byte phases' default rates of the storage they run on (docs/resources.md § Storage kinds):
    /// - the writes waited for (open-sources' second part, its fdatasync, workspace#606; durable-sync) from the
    ///   target's kind, or from its write probe when the kind is unknown and the probe ran (workspace#603, #606): the
    ///   clone ranges from its scattered pass, or its sequential one when that pass was skipped, the fsync from its
    ///   sequential pass (workspace#608);
    /// - the byte copy into the page cache (volume-copy; open-sources' first part) from the source's kind, or
    ///   `source_read` (bytes per second the precheck read the source at) when that is faster: the precheck is often
    ///   CPU-bound, so its read rate is only a floor of what the source reads at (workspace#603);
    /// - unknown as hdd (workspace#561 round 1).
    pub fn phase_specs(&self, specs: &[PhaseSpec], source_read: Option<f64>) -> Vec<PhaseSpec> {
        use StorageKind::*;
        let (s, t) = (self.source.kind, self.target.kind);
        // the byte copy into the page cache: volume-copy, and open-sources' first part
        let copy = match s {
            Hdd | Unknown => default_rates::COPY_BYTES_HDD,
            _ => default_rates::COPY_BYTES,
        };
        let copy = match source_read {
            Some(r) if r > copy.with_workers(1) => one_point(r),
            _ => copy,
        };
        // the writes waited for: open-sources' second part (its fdatasync) and durable-sync
        let (ranges_synced, fsync) = match (self.target_write, t) {
            (Some(w), _) => (
                one_point(match w.scattered {
                    Some(s) => s.rate() * default_rates::CLONED_RANGE_OF_SCATTERED_PROBE,
                    None => w.sequential.rate() * default_rates::CLONED_RANGE_OF_WRITE_PROBE,
                }),
                one_point(w.sequential.rate() * default_rates::FSYNC_OF_WRITE_PROBE),
            ),
            (None, SataSsd) => (
                default_rates::CLONED_RANGE_BYTES_SATA_SSD,
                default_rates::FSYNC_BYTES_SATA_SSD,
            ),
            (None, Hdd | Unknown) => (
                default_rates::CLONED_RANGE_BYTES_HDD,
                default_rates::FSYNC_BYTES_HDD,
            ),
            (None, Nvme) => (
                default_rates::CLONED_RANGE_BYTES,
                default_rates::FSYNC_BYTES,
            ),
        };
        specs
            .iter()
            .map(|&p| match p.name {
                "volume-copy" => PhaseSpec { rate: copy, ..p },
                "open-sources" => PhaseSpec {
                    rate: copy,
                    more: Box::leak(Box::new([(Unit::Bytes, ranges_synced)])),
                    ..p
                },
                "durable-sync" => PhaseSpec { rate: fsync, ..p },
                _ => p,
            })
            .collect()
    }

    /// The source's throughput with `workers` workers: the pair count reads the source heaps (`Progress::observe`).
    pub fn source_device(&self, workers: usize) -> DeviceThroughput {
        DeviceThroughput::of(self.source.kind, workers)
    }

    pub fn json(&self) -> String {
        let pass = |p: write_probe::Pass| {
            format!(
                "\"bytes\": {}, \"seconds\": {:.3}, \"bytes_per_s\": {:.0}",
                p.bytes,
                p.seconds,
                p.rate()
            )
        };
        // the sequential pass at the top level, as before the scattered one (workspace#608)
        let probe = match self.target_write {
            Some(w) => format!(
                "{{{}, \"scattered\": {}}}",
                pass(w.sequential),
                w.scattered
                    .map_or("null".to_string(), |s| format!("{{{}}}", pass(s)))
            ),
            None => "null".to_string(),
        };
        format!(
            "{{\"source\": \"{}\", \"target\": \"{}\", \"temp\": \"{}\", \"target_write_probe\": {probe}}}",
            self.source.kind.name(),
            self.target.kind.name(),
            self.temp.kind.name()
        )
    }
}

/// A one-point default rate of a rate measured in this run: leaked once per run (a few bytes), as the default rate
/// table holds `'static` points.
pub fn one_point(rate: f64) -> DefaultRate {
    DefaultRate {
        points: Box::leak(Box::new([(1, rate)])),
    }
}

/// The storage kind of `path`.
pub fn storage_kind(path: &Path) -> Detected {
    let none = Detected {
        kind: StorageKind::Unknown,
        disks: Vec::new(),
    };
    let Some(dev) = path
        .ancestors()
        .find_map(|a| fs::metadata(a).ok())
        .map(|m| m.dev())
    else {
        return none;
    };
    let sys = PathBuf::from(format!("/sys/dev/block/{}:{}", major(dev), minor(dev)));
    match fs::canonicalize(&sys) {
        Ok(dir) => {
            let mut disks = Vec::new();
            let kind = disk_kind(&dir, &mut disks, 0);
            Detected { kind, disks }
        }
        Err(_) => none,
    }
}

/// glibc `gnu_dev_major`.
fn major(dev: u64) -> u64 {
    ((dev >> 8) & 0xfff) | ((dev >> 32) & !0xfff)
}

/// glibc `gnu_dev_minor`.
fn minor(dev: u64) -> u64 {
    (dev & 0xff) | ((dev >> 12) & !0xff)
}

/// `dir` is a canonical sysfs block directory; the slowest kind of the disks under it, their names pushed to `disks`.
fn disk_kind(dir: &Path, disks: &mut Vec<String>, depth: usize) -> StorageKind {
    // a partition's directory sits inside its disk's
    let dir = match (dir.join("partition").exists(), dir.parent()) {
        (true, Some(disk)) => disk,
        _ => dir,
    };
    let slaves: Vec<PathBuf> = fs::read_dir(dir.join("slaves"))
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).collect())
        .unwrap_or_default();
    // depth bound: a stacked device (dm on md on partitions) is a few levels, a cycle in sysfs is not expected
    if !slaves.is_empty() && depth < 8 {
        return slaves
            .iter()
            .filter_map(|s| fs::canonicalize(s).ok())
            .map(|s| disk_kind(&s, disks, depth + 1))
            .max()
            .unwrap_or(StorageKind::Unknown);
    }
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let rotational =
        fs::read_to_string(dir.join("queue/rotational")).is_ok_and(|r| r.trim() == "1");
    let kind = if rotational {
        StorageKind::Hdd
    } else if name.starts_with("nvme") {
        StorageKind::Nvme
    } else if fs::metadata(dir.join("queue")).is_ok() {
        StorageKind::SataSsd
    } else {
        StorageKind::Unknown
    };
    disks.push(name);
    kind
}

impl RunStorage {
    /// How the precheck reads the source by extents: one stream on a rotating disk, where parallel reads seek
    /// against each other; parallel elsewhere, an unknown device included (workspace#596).
    pub fn source_extent_reads(&self) -> ExtentReads {
        match self.source.kind {
            StorageKind::Hdd => ExtentReads::OneStream,
            _ => ExtentReads::Parallel,
        }
    }
}
