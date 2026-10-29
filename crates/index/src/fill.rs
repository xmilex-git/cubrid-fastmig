//! Fills B-trees created earlier (`btree_file::create_btree_file`) from the rows of their classes: one heap scan
//! over every owner class at once, then every tree built at once, all under the one worker and memory limit.
//!
//! Input: the target's space manager, the [`IndexDef`]s (BTID, owners, shape, key domain), the limits and an
//! optional progress meter (keys extracted). Output: one [`FilledIndex`] per definition, in order — the B-tree
//! statistics the catalog representation stores (workspace#482 G2 item 7) and the overflow key file if one was
//! needed. A unique violation, a duplicate entry, a NULL primary key column or an unreadable row stops the fill.
//! Source: R6 § 2 (engine no-logging parallel load); workspace#503 (two-step API: BTIDs exist before the keys, so
//! class objects and catalog rows can name them first, workspace#504), G1 workspace#481 (`--workers`, `--mem`).

use std::collections::{BTreeMap, HashSet};

use fastmig_ondisk::file_alloc::{FilePages, SpaceManager};
use fastmig_ondisk::file_table::FileTable;
use fastmig_ondisk::{Btid, Oid, Vfid};
use fastmig_progress::Meter;

use crate::bt_stats::BtStats;
use crate::btree_file::create_overflow_key_file;
use crate::build::IndexJob;
use crate::build_error::BuildError;
use crate::build_many::build_indexes;
use crate::heap_keys::{ClassKeys, page_keys};
use crate::heap_scan::{RowKey, ScanClass, scan_classes};
use crate::index_def::IndexDef;
use crate::intermediate_merge::MERGE_FLOOR;
use crate::key_order::KeyOrder;
use crate::memory_limit::MemoryLimit;
use crate::ovf_key_file::MakeSink;
use crate::page_sink::PageSink;
use crate::pair_count::pair_bytes;
use crate::run_writer::IndexRuns;
use crate::spill_dir::SpillDir;
use crate::temp_disk::index_batches;
use crate::volume_sink::VolumeFileSink;
use crate::worker_limit::WorkerLimit;

/// The worker limit, memory limit, spill directory and temp disk limit one run shares (cli `--workers`, `--mem`,
/// `--spill-dir`, `--temp-disk`).
pub struct Limits<'a> {
    pub workers: &'a WorkerLimit,
    pub memory: &'a MemoryLimit,
    pub spill: &'a SpillDir,
    /// Bytes spill files may hold at once; None: one batch, as many spill files as the memory limit leaves.
    pub temp_disk: Option<u64>,
}

/// One filled index.
#[derive(Clone, Debug)]
pub struct FilledIndex {
    pub btid: Btid,
    pub ovfid: Option<Vfid>,
    pub stats: BtStats,
    /// Objects in the tree (live rows with a key).
    pub oids: u64,
    /// Live rows whose key is all NULL (not in the tree).
    pub nulls: u64,
}

/// Fills every definition: in one batch, or with a temp disk limit in batches whose spill bound fits it — the
/// pairs counted first (`pair_count`), cut by `temp_disk::index_batches`, each batch its own heap scan and builds,
/// its spill files gone before the next.
pub fn fill_btrees(
    space: &SpaceManager,
    defs: &[IndexDef],
    lim: &Limits,
    progress: Option<Meter>,
) -> Result<Vec<FilledIndex>, BuildError> {
    let all: Vec<&IndexDef> = defs.iter().collect();
    let batches = match lim.temp_disk {
        None => vec![(0..defs.len()).collect()],
        Some(limit) => {
            let bytes: Vec<u64> = pair_bytes(
                space.volumes(),
                &all,
                lim.workers.limit(),
                &|_| None,
                &HashSet::new(),
            )?
            .iter()
            .map(|by_class| by_class.values().sum())
            .collect();
            index_batches(&bytes, limit).map_err(|min| {
                BuildError::TempDisk(format!(
                    "{limit} B is below {min} B, the pair bytes of the largest index"
                ))
            })?
        }
    };
    let mut out: Vec<Option<FilledIndex>> = vec![None; defs.len()];
    for b in batches {
        let part: Vec<&IndexDef> = b.iter().map(|&i| all[i]).collect();
        for (&i, f) in b.iter().zip(fill_batch(space, &part, lim, progress)?) {
            out[i] = Some(f);
        }
    }
    Ok(out.into_iter().map(Option::unwrap).collect())
}

/// One heap scan over the owner classes of `defs`, then their builds.
fn fill_batch(
    space: &SpaceManager,
    defs: &[&IndexDef],
    lim: &Limits,
    progress: Option<Meter>,
) -> Result<Vec<FilledIndex>, BuildError> {
    let vols = space.volumes();
    // per owner class: its heap and, per index it feeds, (definition, position of its runs)
    let mut by_class: BTreeMap<Oid, (usize, usize, Vec<usize>)> = BTreeMap::new();
    for (d, def) in defs.iter().enumerate() {
        for (o, owner) in def.owners.iter().enumerate() {
            by_class
                .entry(owner.class_oid)
                .or_insert_with(|| (d, o, Vec::new()))
                .2
                .push(d);
        }
    }
    let mut keys = Vec::with_capacity(by_class.len());
    let mut scans = Vec::with_capacity(by_class.len());
    for (&class_oid, (d0, o0, feeds)) in &by_class {
        let owner = &defs[*d0].owners[*o0];
        let indexes = feeds
            .iter()
            .map(|&d| {
                let c = defs[d]
                    .owners
                    .iter()
                    .find(|o| o.class_oid == class_oid)
                    .unwrap()
                    .constraint;
                (c, &defs[d].key_domain)
            })
            .collect();
        keys.push(
            ClassKeys::new(class_oid, owner.class, owner.hfid, indexes).map_err(|e| {
                BuildError::Record {
                    oid: class_oid,
                    what: format!("class layout: {e}"),
                }
            })?,
        );
        scans.push(ScanClass {
            sectors: FileTable::read(vols, owner.hfid.vfid)?.user_pages_by_sector(),
            orders: feeds
                .iter()
                .map(|&d| &defs[d].key_domain as &dyn KeyOrder)
                .collect(),
        });
    }
    let io_pagesize = vols.io_pagesize();
    // merge memory (intermediate_merge): a floor per worker held from before the scan, which cannot take it, and
    // after the scan all the limit has free; the builds' merges split it per worker permit
    let workers = lim.workers.limit();
    let floor = lim.memory.try_acquire(workers * MERGE_FLOOR);
    let class_runs = scan_classes(
        &scans,
        lim.workers,
        lim.memory,
        lim.spill,
        || vec![0u8; io_pagesize].into_boxed_slice(),
        |buf, c, vpid, emit| {
            let mut n = 0u64;
            let r = page_keys(vols, &keys[c], buf, vpid, &mut |ix, oid, key: RowKey| {
                n += 1;
                emit(ix, oid, key)
            });
            if let Some(m) = &progress {
                m.add(n);
            }
            r
        },
    )?;

    // each definition's runs from every owner class
    let mut runs: Vec<IndexRuns> = defs.iter().map(|_| IndexRuns::default()).collect();
    let mut nulls = vec![0u64; defs.len()];
    for ((_, (_, _, feeds)), cr) in by_class.iter().zip(class_runs) {
        for ((&d, run), n) in feeds.iter().zip(cr.runs).zip(cr.nulls) {
            runs[d].append(run);
            nulls[d] += n;
        }
    }

    let sinks: Vec<VolumeFileSink> = defs
        .iter()
        .map(|d| {
            VolumeFileSink::btree(FilePages {
                space,
                vfid: d.btid.vfid,
            })
        })
        .collect();
    let make_ovf: Vec<OvfKeyFileOnDemand> = defs
        .iter()
        .map(|d| OvfKeyFileOnDemand {
            space,
            btid: d.btid,
            class: d.shape.topclass,
        })
        .collect();
    let jobs: Vec<IndexJob> = defs
        .iter()
        .zip(runs)
        .zip(nulls)
        .enumerate()
        .map(|(i, ((d, runs), nulls))| IndexJob {
            root: d.btid.vpid(),
            shape: d.shape.clone(),
            order: &d.key_domain,
            runs,
            nulls,
            sink: &sinks[i],
            make_ovf_key_sink: &make_ovf[i],
        })
        .collect();
    let free = lim.memory.try_acquire(lim.memory.limit());
    let merge_budget = (floor.bytes + free.bytes) / workers.max(1);
    defs.iter()
        .zip(build_indexes(jobs, lim.workers, lim.spill, merge_budget))
        .map(|(d, built)| {
            let b = built?;
            Ok(FilledIndex {
                btid: d.btid,
                ovfid: b.ovfid,
                stats: b.stats,
                oids: b.oids,
                nulls: b.nulls,
            })
        })
        .collect()
}

/// The overflow key file of one B-tree in the target, created when its first key overflows.
struct OvfKeyFileOnDemand<'s, 'v> {
    space: &'s SpaceManager<'v>,
    btid: Btid,
    class: Oid,
}

impl MakeSink for OvfKeyFileOnDemand<'_, '_> {
    fn make(&self) -> std::io::Result<Box<dyn PageSink + Send + '_>> {
        let vfid = create_overflow_key_file(self.space, self.btid, self.class)?;
        Ok(Box::new(VolumeFileSink::overflow_key(FilePages {
            space: self.space,
            vfid,
        })))
    }
}
