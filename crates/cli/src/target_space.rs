//! The target space a migrate takes before its first write (workspace#526: no disk full during a run): the
//! byte-copied volumes (`volume_copy::preallocate_targets`) and the new log (`new_log::allocate`) are allocated in
//! full, and one reservation file holds an upper bound of everything else the run may write — pages a cloned
//! volume unshares, new B-trees, LOB and Java files — until the run ends (held twice meanwhile, which 1번 세션
//! accepted: the free-space check needs twice the reservation). Those writes go outside the space the run took, so
//! a process filling the filesystem meanwhile can still take it; `plan` shows their bound as unreserved writes.
//!
//! Input: the plan, the resource estimate, the IO page size, the size of the new log. Output: [`TargetSpace`] and
//! the [`Reservation`] file, a scratch line of the marker (a rerun removes it, the fsync skips it), removed on drop.
//! Bound of the rewritten pages (`Plan::rewritten_pages_bound`): every heap page when the catalog is regenerated,
//! else the treated pages and the root heap. Bound of the new B-trees: [`btree_pages`] per index, from the builder's
//! rules and the index's pairs, keys and overflow OID chains (the source's own tree, `verify::source_key_groups`).
//! The former `5 · T_i / page + 4` was no bound: a key of 86 rows takes a 16 KiB overflow OID page for one object.
//! Source: workspace#526 (1번 세션 decision: copy volumes allocated at the start, one reservation for the rest).

use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};

use fastmig_copy::IncompleteMarker;
use fastmig_index::btree_page::BtreePage;
use fastmig_index::build::MIN_ENTRIES_PER_SHARD;
use fastmig_index::index_shape::{DEFAULT_UNFILL_FACTOR, IndexShape};
use fastmig_index::temp_disk::spill_record_bytes;
use fastmig_ondisk::btree::node_header::NodeHeader;
use fastmig_ondisk::btree::ovf_oid_page::OvfHeader;
use fastmig_ondisk::btree::{
    BTREE_MAX_ALIGN, DISK_VPID_SIZE, max_keylen_inpage, max_oidlen_inpage, object_fixed_size,
    ovf_key,
};
use fastmig_ondisk::spage::{SPAGE_HEADER_SIZE, SPAGE_SLOT_SIZE};
use fastmig_ondisk::{Oid, Vpid, db_pagesize, preallocate};
use fastmig_plan::{CopyMethod, Plan};
use fastmig_verify::btree_walk::LeafCounts;

use crate::resource_estimate::{DefFigures, ResourceEstimate};

/// Pages for the catalog file and class records a run writes beyond the bounds above.
const CATALOG_SLACK_PAGES: u64 = 64;

#[derive(Clone, Copy, Debug)]
pub struct TargetSpace {
    /// Byte-copied volumes, allocated at the start.
    pub copy_bytes: u64,
    /// The new log, allocated at the start.
    pub log_bytes: u64,
    /// The reservation file: everything else the run may write, outside the space allocated at the start (the
    /// unreserved writes).
    pub extra_bytes: u64,
    /// The new B-trees' part of it, in pages (`btree_pages` summed).
    pub btree_pages: u64,
}

impl TargetSpace {
    /// Free space the run needs on the target filesystem: the reservation stays until the run ends while the writes
    /// it bounds take up to as much again.
    pub fn needed(&self) -> u64 {
        self.copy_bytes + self.log_bytes + 2 * self.extra_bytes
    }
}

pub fn bound(plan: &Plan, est: &ResourceEstimate, io_page: u64, new_log_bytes: u64) -> TargetSpace {
    let copy_bytes = plan
        .volumes
        .iter()
        .filter(|v| v.copy_method == CopyMethod::Copy)
        .map(|v| v.bytes)
        .sum();
    let btree_pages: u64 = est
        .per_def_figures
        .iter()
        .map(|f| {
            let g = f
                .from_heaps
                .then(|| est.source_groups.get(&(f.class.clone(), f.index.clone())))
                .flatten();
            btree_pages(f, g, io_page as usize, est.workers)
        })
        .sum();
    let pages = plan.rewritten_pages_bound + btree_pages + est.appended_pages + CATALOG_SLACK_PAGES;
    let extra_bytes =
        pages * io_page + plan.lob.referenced_bytes() + plan.java.bytes + plan.java_static.bytes;
    TargetSpace {
        copy_bytes,
        log_bytes: new_log_bytes,
        extra_bytes,
        btree_pages,
    }
}

/// Pages the rebuild of one index writes at most, from the builder's own rules (`leaf_builder`, `nonleaf_builder`,
/// `ovf_oid_chain`, `ovf_key_writer`; R6 § 4.3–4.4) and what is known of its key groups.
///
/// Keys `D` and overflow OID chains `C` (objects `P` past a leaf record) come from the source's own tree when it
/// indexes the counted rows (`g`; every live row's key is in it, deleted ones too), else the worst case: every
/// pair its own key, every 86th pair opening a chain. A leaf record takes its first object (8 B, or the fixed
/// object when it has a chain), the key (or a 6 B overflow link), 3 B of padding, the 8 B (unique: 16 B) of every
/// other object in it, an 8 B chain link, alignment to 4 and its 4 B slot. A leaf takes no record that would
/// leave less than `leaf_reserve` free, so it holds at least its user area minus the reserve minus the largest
/// record; one partial leaf per shard. A chain fills data pages of `objects_per_page` objects, its last one
/// partial, and directory pages. Every leaf but the first has one separator above it (the child link, a 2 B
/// length and the key or its link; an overflow separator writes its own copy of the key), filled the same way with
/// `nonleaf_reserve`, level by level up to the root. Plus the B-tree and overflow key files' header and table
/// pages.
pub fn btree_pages(f: &DefFigures, g: Option<&LeafCounts>, io_page: usize, workers: usize) -> u64 {
    let s = &f.stats;
    if s.pairs == 0 {
        return 4;
    }
    let db = db_pagesize(io_page);
    let n = s.pairs as f64;
    let key_sum = (s.bytes - spill_record_bytes(0) * s.pairs) as f64;
    let in_page = max_keylen_inpage(db);
    // the longest key a record holds inline; a longer one leaves a 6 B link
    let key_part = (s.max_key as usize).min(in_page - 1) as f64;
    let fixed = object_fixed_size(f.unique) as f64;
    let other = if f.unique { 16.0 } else { 8.0 };
    let per_record = (max_oidlen_inpage(db) / object_fixed_size(f.unique)) as f64;
    let (d, c, p) = match g {
        Some(g) => (
            (g.records as f64).min(n),
            g.wide_records as f64,
            g.objects_past_record as f64,
        ),
        None => (n, (n / (per_record + 1.0)).floor(), n),
    };
    let leaf_bytes = key_sum + d * (8.0 + 3.0 + 3.0 + 4.0) + (n - d) * other + c * (fixed + 8.0);
    let area = (db - SPAGE_HEADER_SIZE) / BTREE_MAX_ALIGN * BTREE_MAX_ALIGN
        - (NodeHeader::SIZE + SPAGE_SLOT_SIZE);
    let largest = ((fixed + key_part + 3.0) / 4.0).ceil() * 4.0
        + (per_record - 1.0) * other
        + 8.0
        + SPAGE_SLOT_SIZE as f64;
    let shape = IndexShape {
        unique: f.unique,
        primary_key: false,
        topclass: Oid::NULL,
        packed_key_domain: Vec::new(),
        io_pagesize: io_page,
        unfill_factor: DEFAULT_UNFILL_FACTOR,
    };
    let fill = |reserve: f64, largest: f64| (area as f64 - reserve - largest).max(largest);
    // one partial leaf per shard: the builder adds a shard per MIN_ENTRIES_PER_SHARD entries, up to the workers
    let shards = (s.pairs as usize / MIN_ENTRIES_PER_SHARD).clamp(1, workers.max(1)) as f64;
    let leaves = (leaf_bytes / fill(shape.leaf_reserve(), largest)).ceil() + shards;
    let opp = {
        let mut pg = BtreePage::new(Vpid::NULL, db);
        pg.push(vec![0; OvfHeader::DIR_SIZE]);
        (pg.max_space_for_new_record() / object_fixed_size(f.unique)) as f64
    };
    let chain_data = (p / opp).ceil() + c;
    let chain_dirs = c + (chain_data / 100.0).ceil();
    // non-leaf levels: one separator per child, a level per pass until one page (the root) remains
    let sep = ((DISK_VPID_SIZE as f64 + 2.0 + key_part.max(DISK_VPID_SIZE as f64) + 3.0) / 4.0)
        .ceil()
        * 4.0
        + SPAGE_SLOT_SIZE as f64;
    let mut upper = 0.0;
    let mut separators = 0.0;
    let mut level = leaves;
    while level > 1.0 {
        separators += level;
        // the builder fills a level in one pass: one partial page per level
        level = (level * sep / fill(shape.nonleaf_reserve(), sep)).ceil();
        upper += level;
    }
    let ovf_keys = if s.max_key as usize >= in_page {
        s.ovf_key_pages as f64 + separators * ovf_key::pages_for(s.max_key as usize, db) as f64
    } else {
        0.0
    };
    (leaves + chain_data + chain_dirs + upper + 1.0 + ovf_keys + 4.0).ceil() as u64
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
