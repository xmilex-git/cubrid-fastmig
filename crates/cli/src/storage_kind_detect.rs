//! Finds the storage kind of the device a path lives on, from sysfs.
//!
//! Input: a path (it need not exist yet: its nearest existing ancestor is used). Output: [`Detected`], a
//! [`StorageKind`] and the block device names it was read from (for the plan).
//! A partition is judged by its whole disk; a device-mapper or md device by its slaves, the slowest of them.
//! A path without a block device behind it (tmpfs, NFS, overlay) or an unreadable sysfs is [`StorageKind::Nvme`],
//! the kind every rate before workspace#561 was measured on.
//! Source: Linux `Documentation/ABI/stable/sysfs-block` (`/sys/dev/block/<major>:<minor>`, `partition`, `slaves/`,
//! `queue/rotational`); glibc `gnu_dev_major`/`gnu_dev_minor` for the dev_t split; workspace#561.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use fastmig_progress::StorageKind;

/// The storage kind of one directory and the whole disks it was judged by (empty when sysfs had none).
#[derive(Clone, Debug)]
pub struct Detected {
    pub kind: StorageKind,
    pub disks: Vec<String>,
}

impl Detected {
    /// `hdd (sdc)`, `nvme (no block device)`.
    pub fn describe(&self) -> String {
        let disks = if self.disks.is_empty() {
            "no block device".to_string()
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
}

impl RunStorage {
    /// The source is judged by every volume path (volumes may sit in several directories): the slowest of them.
    pub fn detect(source_volumes: &[PathBuf], target_dir: &Path, spill_dir: &Path) -> RunStorage {
        let mut source = Detected {
            kind: StorageKind::Nvme,
            disks: Vec::new(),
        };
        for d in source_volumes.iter().map(|v| storage_kind(v)) {
            source.kind = source.kind.max(d.kind);
            for n in d.disks {
                if !source.disks.contains(&n) {
                    source.disks.push(n);
                }
            }
        }
        RunStorage {
            source,
            target: storage_kind(target_dir),
            temp: storage_kind(spill_dir),
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

    pub fn json(&self) -> String {
        format!(
            "{{\"source\": \"{}\", \"target\": \"{}\", \"temp\": \"{}\"}}",
            self.source.kind.name(),
            self.target.kind.name(),
            self.temp.kind.name()
        )
    }
}

/// The storage kind of `path`.
pub fn storage_kind(path: &Path) -> Detected {
    let none = Detected {
        kind: StorageKind::Nvme,
        disks: Vec::new(),
    };
    let Some(dev) = path.ancestors().find_map(|a| fs::metadata(a).ok()).map(|m| m.dev()) else {
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
            .unwrap_or(StorageKind::Nvme);
    }
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let rotational = fs::read_to_string(dir.join("queue/rotational")).is_ok_and(|r| r.trim() == "1");
    let kind = if rotational {
        StorageKind::Hdd
    } else if name.starts_with("nvme") {
        StorageKind::Nvme
    } else if fs::metadata(dir.join("queue")).is_ok() {
        StorageKind::SataSsd
    } else {
        StorageKind::Nvme
    };
    disks.push(name);
    kind
}
