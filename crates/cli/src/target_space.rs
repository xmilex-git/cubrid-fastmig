//! The target space a migrate takes before its first write (workspace#526: no disk full during a run): the
//! byte-copied volumes are allocated in full (`volume_copy::preallocate_targets`), and one reservation file holds an
//! upper bound of everything else the run may write — pages a cloned volume unshares, new B-trees, added volumes,
//! LOB and Java files, the new log — until the run ends (held twice meanwhile, which 1번 세션 accepted: the
//! free-space check needs twice the reservation).
//!
//! Input: the plan, the resource estimate, the IO page size, the size of the new log. Output: [`TargetSpace`] and
//! the [`Reservation`] file, a scratch line of the marker (a rerun removes it, the fsync skips it), removed on drop.
//! Bound of the rewritten pages (`Plan::rewritten_pages_bound`): every heap page when the catalog is regenerated,
//! else the treated pages and the root heap. Bound of the new B-trees: a leaf record of a pair takes at most twice
//! its spill record (`12 + key`: key, OID, class OID, slot), leaves are filled to at most 90 % and non-leaf pages are
//! fewer than leaves, so 5 · T_i / page + 4 pages per index.
//! Source: workspace#526 (1번 세션 decision: copy volumes allocated at the start, one reservation for the rest).

use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};

use fastmig_copy::IncompleteMarker;
use fastmig_ondisk::preallocate;
use fastmig_plan::{CopyMethod, Plan};

use crate::resource_estimate::ResourceEstimate;

/// Pages for the catalog file and class records a run writes beyond the bounds above.
const CATALOG_SLACK_PAGES: u64 = 64;

#[derive(Clone, Copy, Debug)]
pub struct TargetSpace {
    /// Byte-copied volumes, allocated at the start.
    pub copy_bytes: u64,
    /// The reservation file: everything else the run may write.
    pub extra_bytes: u64,
}

impl TargetSpace {
    /// Free space the run needs on the target filesystem: the reservation stays until the run ends while the writes
    /// it bounds take up to as much again.
    pub fn needed(&self) -> u64 {
        self.copy_bytes + 2 * self.extra_bytes
    }
}

pub fn bound(plan: &Plan, est: &ResourceEstimate, io_page: u64, new_log_bytes: u64) -> TargetSpace {
    let copy_bytes = plan
        .volumes
        .iter()
        .filter(|v| v.copy_method == CopyMethod::Copy)
        .map(|v| v.bytes)
        .sum();
    let btree_pages: u64 = est.per_def.iter().map(|&t| 5 * t / io_page + 4).sum();
    let pages = plan.rewritten_pages_bound + btree_pages + CATALOG_SLACK_PAGES;
    let extra_bytes = pages * io_page
        + plan.lob.referenced_bytes()
        + plan.java.bytes
        + plan.java_static.bytes
        + new_log_bytes;
    TargetSpace {
        copy_bytes,
        extra_bytes,
    }
}

/// A file allocated in full that only holds space; removed on drop.
pub struct Reservation {
    path: PathBuf,
}

impl Reservation {
    pub fn take(marker: &IncompleteMarker, path: &Path, bytes: u64) -> io::Result<Self> {
        marker.record_scratch(path)?;
        let f = OpenOptions::new().write(true).create_new(true).open(path)?;
        let r = Reservation {
            path: path.to_path_buf(),
        };
        preallocate::allocate(&f, bytes).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!("reserving {bytes} B at {}: {e}", path.display()),
            )
        })?;
        Ok(r)
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
