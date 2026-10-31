//! The storage kind of a directory's device: what the default rates and the I/O concurrency are chosen by.
//!
//! Input: none (the cli reads sysfs and picks one, `storage_kind_detect`). Output: [`StorageKind`].
//! Source: workspace#561 (NVMe, SATA SSD and HDD measured with the same DB; rotational alone cannot tell NVMe from a
//! SATA SSD, so the device name decides that).

/// One device class whose throughput the default rates were measured on, or [`StorageKind::Unknown`].
/// Declared in the order a mix of devices resolves to (`max`): the slowest known kind, and `Unknown` beats every
/// kind but `Hdd` (one device known to be slow is enough to pick it).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum StorageKind {
    /// An NVMe block device (`nvme*`).
    Nvme,
    /// A non-rotational device that is not NVMe (SATA or SAS SSD).
    SataSsd,
    /// No block device found behind the path (btrfs and ZFS report an anonymous device, so do tmpfs and network
    /// filesystems) or sysfs unreadable; its defaults are chosen conservatively (workspace#561).
    Unknown,
    /// A rotational device (`queue/rotational` = 1).
    Hdd,
}

impl StorageKind {
    pub fn name(self) -> &'static str {
        match self {
            StorageKind::Nvme => "nvme",
            StorageKind::SataSsd => "sata-ssd",
            StorageKind::Unknown => "unknown",
            StorageKind::Hdd => "hdd",
        }
    }
}
