//! The target space a migrate takes before its first write (workspace#526: no disk full during a run): the
//! byte-copied volumes (`volume_copy::preallocate_targets`) and the new log (`new_log::allocate`) are allocated in
//! full; on XFS the pages and sectors (listed sectors, ADR 0013) the run will write in a cloned volume are unshared;
//! and one reservation file holds an upper bound of everything else the run may write — sectors off the list, an
//! expansion, LOB and Java files; elsewhere also the rewritten pages and the new B-trees — until the run ends
//! (held twice meanwhile, which 1번 세션 accepted: the free-space check needs twice the reservation). Those writes
//! go outside the space the run took, so a process filling the filesystem meanwhile can still take it; `plan` shows
//! their bound as unreserved writes.
//!
//! Input: the plan, the resource estimate, the IO page size, the size of the new log. Output: [`TargetSpace`] and
//! the [`Reservation`] file, a scratch line of the marker (a rerun removes it, the fsync skips it), removed on drop.
//! Bound of the rewritten pages (`Plan::rewritten_pages_bound`): every live heap page and its bestspace pages when
//! the catalog is regenerated, else the treated pages and the root heap; every overflow heap and tracker page.
//! Bound of the new B-trees: [`btree_pages`] per index, from the builder's rules and the index's pairs, keys and
//! overflow OID chains (the source's own tree, `verify::source_key_groups`).
//! The former `5 · T_i / page + 4` was no bound: a key of 86 rows takes a 16 KiB overflow OID page for one object.
//! Source: workspace#526 (1번 세션 decision: copy volumes allocated at the start, one reservation for the rest).

use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

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
use fastmig_ondisk::sector_reserve;
use fastmig_ondisk::sector_table::SECTOR_NPAGES;
use fastmig_ondisk::spage::{SPAGE_HEADER_SIZE, SPAGE_SLOT_SIZE};
use fastmig_ondisk::volume_set::VolumeSet;
use fastmig_ondisk::{Oid, VersionLine, Vpid, Vsid, db_pagesize, preallocate};
use fastmig_plan::migration_plan::PageRange;
use fastmig_plan::{CopyMethod, Plan};
use fastmig_verify::btree_walk::LeafCounts;

use crate::resource_estimate::{DefFigures, ResourceEstimate};

/// Pages for the catalog file and class records a run writes beyond the bounds above.
const CATALOG_SLACK_PAGES: u64 = 64;

/// Sectors reserved besides 2 % of the listed ones for sectors a run takes off the list.
const OFF_LIST_SECTORS: u64 = 8;

#[derive(Clone, Debug)]
pub struct TargetSpace {
    /// Byte-copied volumes, allocated at the start.
    pub copy_bytes: u64,
    /// The new log, allocated at the start.
    pub log_bytes: u64,
    /// Cloned pages the run writes, unshared at the start (`unshare_ranges`; 0 where the target cannot).
    pub unshare_bytes: u64,
    /// The reservation file: everything else the run may write, outside the space allocated at the start (the
    /// unreserved writes).
    pub extra_bytes: u64,
    /// The new B-trees, in pages (`btree_pages` summed).
    pub btree_pages: u64,
    /// The sectors the run takes in the order the disk cache hands them out (`sector_reserve::next_sectors`), listed
    /// at the start where the target can unshare; a sector reserved off the list is claimed when reserved.
    pub sectors: Vec<Vsid>,
    /// Sectors the free ones cannot give: an expansion, reserved.
    pub short_sectors: u64,
    /// Cloned pages unshared at the start: the rewritten pages (`Plan::unshare_ranges`) and each cloned volume's
    /// header and sector table; merged.
    pub unshare_ranges: Vec<PageRange>,
    /// Cloned pages given blocks of their own without a copy at the start, as unallocated pages
    /// (`preallocate::unallocated_pages`): the listed sectors on cloned volumes and the plan's `zero_ranges`; merged.
    pub zero_ranges: Vec<PageRange>,
    /// Listed sectors on cloned volumes of files the run frees (and reads until then): given their blocks as unallocated
    /// pages when reserved again, the space handed over from the reservation.
    pub deferred: Vec<Vsid>,
    /// Their bytes, held in the reservation besides `extra_bytes` and taken once.
    pub transfer_bytes: u64,
}

impl TargetSpace {
    /// Free space the run needs on the target filesystem: the reservation stays until the run ends while the writes
    /// it bounds take up to as much again.
    pub fn needed(&self) -> u64 {
        self.copy_bytes
            + self.log_bytes
            + self.unshare_bytes
            + self.transfer_bytes
            + 2 * self.extra_bytes
    }
}

pub fn bound(
    plan: &Plan,
    est: &ResourceEstimate,
    vols: &VolumeSet,
    new_log_bytes: u64,
) -> io::Result<TargetSpace> {
    let io_page = vols.io_pagesize() as u64;
    let copy_bytes = plan
        .volumes
        .iter()
        .filter(|v| v.copy_method == CopyMethod::Copy)
        .map(|v| v.bytes)
        .sum();
    let in_page = max_keylen_inpage(db_pagesize(io_page as usize));
    let per_index: Vec<(u64, bool)> = est
        .per_def_figures
        .iter()
        .map(|f| {
            let g = f
                .from_heaps
                .then(|| est.source_groups.get(&(f.class.clone(), f.index.clone())))
                .flatten();
            (
                btree_pages(f, g, io_page as usize, est.workers),
                f.stats.max_key as usize >= in_page,
            )
        })
        .collect();
    let btree_pages: u64 = per_index.iter().map(|(p, _)| p).sum();
    let mut t = TargetSpace {
        copy_bytes,
        log_bytes: new_log_bytes,
        unshare_bytes: 0,
        extra_bytes: plan.lob.referenced_bytes() + plan.java.bytes + plan.java_static.bytes,
        btree_pages,
        sectors: Vec::new(),
        short_sectors: 0,
        unshare_ranges: Vec::new(),
        zero_ranges: Vec::new(),
        deferred: Vec::new(),
        transfer_bytes: 0,
    };
    if !plan.unshare {
        // rewritten and new pages: inside a byte-copied volume, outside a cloned one this filesystem cannot unshare
        let pages =
            plan.rewritten_pages_bound + btree_pages + est.appended_pages + CATALOG_SLACK_PAGES;
        t.extra_bytes += pages * io_page;
        return Ok(t);
    }
    // the sectors the run takes, listed now and unshared at the start with the rewritten pages: per index its
    // B-tree file (and overflow key file), the appended heap pages and the catalog slack, the new catalog files
    let n: u64 = per_index
        .iter()
        .map(|&(p, ovf)| file_sectors(p) + ovf as u64)
        .sum::<u64>()
        + (est.appended_pages + CATALOG_SLACK_PAGES).div_ceil(SECTOR_NPAGES as u64)
        + plan.catalog_sectors;
    let freed: HashSet<Vsid> = plan.freed_sectors.iter().copied().collect();
    let (sectors, short) = sector_reserve::next_sectors(vols, &freed, n as usize)?;
    let cloned: Vec<i16> = plan
        .volumes
        .iter()
        .filter(|v| v.copy_method == CopyMethod::Clone)
        .map(|v| v.volid)
        .collect();
    let mut ranges = plan.unshare_ranges.clone();
    for v in vols.volumes().iter().filter(|v| cloned.contains(&v.volid)) {
        // the volume header and sector table: the run sets sector bits, an expansion rewrites both
        ranges.push(PageRange {
            volid: v.volid,
            first: 0,
            pages: 1,
        });
        ranges.push(PageRange {
            volid: v.volid,
            first: v.header.stab_first_page,
            pages: v.header.stab_npages as u32,
        });
    }
    // the listed sectors' content is not needed (disjoint from the pages above, which belong to live files and the
    // volume's system pages): a free one gets its blocks now; one of a file the run frees is read until then, so it
    // gets them when reserved again, from the reservation (`deferred`)
    let (deferred, free): (Vec<Vsid>, Vec<Vsid>) = sectors
        .iter()
        .copied()
        .filter(|s| cloned.contains(&s.volid))
        .partition(|s| freed.contains(s));
    let mut zero = plan.zero_ranges.clone();
    zero.extend(free.iter().map(|s| PageRange {
        volid: s.volid,
        first: s.sectid * SECTOR_NPAGES,
        pages: SECTOR_NPAGES as u32,
    }));
    t.unshare_ranges = merge_ranges(ranges);
    t.zero_ranges = merge_ranges(zero);
    t.transfer_bytes = deferred.len() as u64 * SECTOR_NPAGES as u64 * io_page;
    t.deferred = deferred;
    t.unshare_bytes = (t.unshare_ranges.iter().chain(&t.zero_ranges))
        .map(|r| r.pages as u64)
        .sum::<u64>()
        * io_page;
    // reserved: what the free sectors cannot give, as the engine's expansion takes it (1 % of the data sectors or
    // 64, plus the request), and room for sectors taken off the list (a request of more than one sector)
    let total: u64 = vols
        .volumes()
        .iter()
        .filter(|v| v.header.purpose == 0 && v.header.vol_type == 0)
        .map(|v| v.header.nsect_total as u64)
        .sum();
    let expansion = if short > 0 {
        (total / 100).max(64) + short as u64
    } else {
        0
    };
    let off_list = OFF_LIST_SECTORS + n / 50;
    t.extra_bytes += (expansion + off_list) * SECTOR_NPAGES as u64 * io_page;
    t.sectors = sectors;
    t.short_sectors = short as u64;
    Ok(t)
}

/// Sectors a permanent file of `pages` pages holds: one at its creation, then `file_perm_expand` adds 1 % of its
/// sectors (at least one) each time it is full.
fn file_sectors(pages: u64) -> u64 {
    let mut s: u64 = 1;
    while s * (SECTOR_NPAGES as u64) < pages {
        s += (s / 100).max(1);
    }
    s
}

/// `ranges` sorted, overlapping and adjacent ones merged.
fn merge_ranges(mut ranges: Vec<PageRange>) -> Vec<PageRange> {
    ranges.sort_by_key(|r| (r.volid, r.first));
    let mut out: Vec<PageRange> = Vec::new();
    for r in ranges {
        match out.last_mut() {
            Some(l) if l.volid == r.volid && l.first as i64 + l.pages as i64 >= r.first as i64 => {
                let end = (l.first as i64 + l.pages as i64).max(r.first as i64 + r.pages as i64);
                l.pages = (end - l.first as i64) as u32;
            }
            _ => out.push(r),
        }
    }
    out
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
        root_off68: 0,
        // an upper bound for an 11.4 target too: develop's 16 B overflow header and directory need more pages
        target: VersionLine::Develop,
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

/// A file allocated in full that only holds space; removed on drop. [`Reservation::give`] hands its space to a range
/// the run takes, from the start of the file on.
pub struct Reservation {
    path: PathBuf,
    file: File,
    size: u64,
    given: AtomicU64,
}

impl Reservation {
    pub fn take(marker: &IncompleteMarker, path: &Path, bytes: u64) -> io::Result<Self> {
        marker.record_scratch(path)?;
        let file = OpenOptions::new().write(true).create_new(true).open(path)?;
        let r = Reservation {
            path: path.to_path_buf(),
            file,
            size: bytes,
            given: AtomicU64::new(0),
        };
        preallocate::allocate(&r.file, bytes).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!("reserving {bytes} B at {}: {e}", path.display()),
            )
        })?;
        Ok(r)
    }

    /// Gives `len` bytes of the reservation back to the filesystem for the caller to take at once; nothing when less
    /// than that is left.
    // shortcut: another process may take the bytes between the release and the caller's allocation (two syscalls);
    // upgrade to a single exchange if a run on a contended filesystem fails there.
    pub fn give(&self, len: u64) -> io::Result<()> {
        let off = self.given.fetch_add(len, Ordering::Relaxed);
        if off + len > self.size {
            return Ok(());
        }
        preallocate::release(&self.file, off, len)
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
