//! The plan's resource table (workspace#526): the temp disk (spill) and memory the index rebuild needs, so the user
//! picks `--temp-disk` and `--mem` before anything is written.
//!
//! Input: the source volumes, the version pair, the worker limit. Output: [`ResourceEstimate`] — per index the bytes
//! its (key, OID) pairs take as spill records (counted by reading the source heaps with the build's own key
//! extractor, `fastmig_index::pair_count`; an 11.4 source's rows re-encoded first, its system catalog indexes an
//! allowance), the counting scan's seconds, and from those, per memory limit: temp min/max, the index batches a temp
//! limit needs and their extra time.
//! Bounds (docs/resources.md): temp_min(mem) = the largest single index's `spill_bound`, temp_max(mem) = the
//! bound of all indexes in one batch (what one scan of every index spills at most); a batch is one more heap scan,
//! priced at the counting scan's time. mem_recommended spills nothing at full parallelism; mem_min is what the copy
//! engine needs (one I/O buffer per worker) — the rest of the run degrades to spill under a smaller limit.
//! Temp disk the verifier needs comes after the build's spill is gone: a run needs max(build, verify), not the sum.
//! Source: workspace#526 (user requests: the sizes before the start, no disk full in the middle).

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io;
use std::time::Instant;

use fastmig_catalog::class_rewrite::{RewriteInputs, rewrite_user_class};
use fastmig_catalog::classify::ClassKind;
use fastmig_catalog::same_version_catalog::DevelopCatalog;
use fastmig_catalog::source_catalog::SourceCatalog;
use fastmig_catalog::source_classes::SourceClass;
use fastmig_copy::volume_copy::IO_BUF_BYTES;
use fastmig_index::IndexDef;
use fastmig_index::pair_count::pair_bytes;
use fastmig_index::rebuild::{TargetClass, class_constraints, plain_index_defs};
use fastmig_index::temp_disk::{WORKER_SHARE, index_batches, spill_bound};
use fastmig_ondisk::class_object::class_properties;
use fastmig_ondisk::file_table::FileTable;
use fastmig_ondisk::heap_rewrite::{ClassReEncoder, ReEncode};
use fastmig_ondisk::volume_set::VolumeSet;
use fastmig_ondisk::{Oid, SourceVersion, VersionLine};
use fastmig_plan::version_pair::VersionPair;

/// One index's pairs, by its owner class (a BTID shared by classes has one row per class).
pub struct IndexPairs {
    pub class: String,
    pub index: String,
    pub bytes: u64,
}

pub struct ResourceEstimate {
    pub indexes: Vec<IndexPairs>,
    /// Pair bytes per index definition (owners summed): the input of the batching.
    pub per_def: Vec<u64>,
    /// Seconds of the counting scan: one read of every indexed heap, the price of one more batch.
    pub scan_seconds: f64,
    pub workers: usize,
    pub physical_memory: u64,
}

fn invalid(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(format!("resource estimate: {e}"))
}

/// The estimate of a source: a develop source's classes are counted as they are; an 11.4 source's user classes are
/// counted through the class objects the run writes (`rewrite_user_class`, BTIDs kept) and, for a re-encoded class,
/// through its re-encoder, so every key has the length the target will hold. The 11.4 system catalog indexes are
/// rebuilt from regenerated rows the source does not hold; they get an allowance (below).
pub fn estimate(
    vols: &VolumeSet,
    pair: VersionPair,
    source: SourceVersion,
    workers: usize,
    physical_memory: u64,
) -> io::Result<ResourceEstimate> {
    let started = Instant::now();
    let mut e = match pair.source {
        VersionLine::Develop => {
            let cat = DevelopCatalog::read(vols)?;
            let classes: Vec<TargetClass> = cat
                .classes
                .iter()
                .filter(|c| !c.class.heap.is_null())
                .map(|c| TargetClass {
                    oid: c.oid,
                    class: &c.class,
                    hfid: c.class.heap,
                })
                .collect();
            count(vols, &classes, &HashMap::new(), workers)?
        }
        VersionLine::V11_4 => {
            let src = SourceCatalog::read(vols, source)?;
            let mut users = Vec::new();
            let mut re_encoders = HashMap::new();
            let mut allowance = Vec::new();
            for (c, p) in src.classes.iter().zip(&src.plans) {
                if c.class.heap.is_null() {
                    continue;
                }
                if p.kind != ClassKind::User {
                    allowance.extend(system_allowance(vols, c, &p.name)?);
                    continue;
                }
                let target = rewrite_user_class(
                    c,
                    p,
                    &RewriteInputs {
                        rep_dir: Oid::NULL,
                        new_btid: &Some,
                    },
                )
                .map_err(|e| invalid(format!("{}: {e}", p.name)))?;
                if p.re_encoded {
                    // user classes are MVCC classes (page_treatment); LOB locators are not keys
                    re_encoders.insert(
                        c.oid,
                        ClassReEncoder::new(&c.class, true)
                            .map_err(|e| invalid(format!("{}: {e}", p.name)))?,
                    );
                }
                users.push((c.oid, target));
            }
            let classes: Vec<TargetClass> = users
                .iter()
                .map(|(oid, class)| TargetClass {
                    oid: *oid,
                    class,
                    hfid: class.heap,
                })
                .collect();
            let mut e = count(vols, &classes, &re_encoders, workers)?;
            for a in allowance {
                e.per_def.push(a.bytes);
                e.indexes.push(a);
            }
            e
        }
    };
    e.scan_seconds = started.elapsed().as_secs_f64();
    e.workers = workers;
    e.physical_memory = physical_memory;
    Ok(e)
}

/// Allowance for the indexes of an 11.4 system class, which the run fills from regenerated catalog rows: per index
/// twice the class's heap bytes (a catalog row is small and every pair's key is at most its row, `12 +` key at
/// most doubles it).
fn system_allowance(vols: &VolumeSet, c: &SourceClass, name: &str) -> io::Result<Vec<IndexPairs>> {
    if c.class.properties.is_empty() {
        return Ok(Vec::new());
    }
    let heap_bytes = FileTable::read(vols, c.class.heap.vfid)?.user_pages().len() as u64
        * vols.io_pagesize() as u64;
    let cons = class_properties(&c.class.properties, VersionLine::V11_4)
        .map_err(|e| invalid(format!("{name}: {e}")))?
        .constraints;
    Ok(cons
        .iter()
        .filter(|k| k.btid.is_some())
        .map(|k| IndexPairs {
            class: name.to_string(),
            index: format!("{} (allowance)", String::from_utf8_lossy(&k.name)),
            bytes: 2 * heap_bytes,
        })
        .collect())
}

/// Counts the pairs of every plain index of `classes`, each class's records through its re-encoder if it has one.
fn count(
    vols: &VolumeSet,
    classes: &[TargetClass],
    re_encoders: &HashMap<Oid, ClassReEncoder>,
    workers: usize,
) -> io::Result<ResourceEstimate> {
    let cons = class_constraints(classes).map_err(invalid)?;
    let charset = vols.volume(0)?.header.db_charset as u8;
    let (defs, _) =
        plain_index_defs(classes, &cons, vols.io_pagesize(), charset).map_err(invalid)?;
    let refs: Vec<&IndexDef> = defs.iter().collect();
    let counted = pair_bytes(vols, &refs, workers, &|oid| {
        re_encoders.get(&oid).map(|r| r as &dyn ReEncode)
    })
    .map_err(invalid)?;
    let name_of = |oid| {
        classes.iter().find(|c| c.oid == oid).map_or_else(
            || format!("{oid:?}"),
            |c| String::from_utf8_lossy(&c.class.name).into(),
        )
    };
    let mut indexes = Vec::new();
    for (d, by_class) in defs.iter().zip(&counted) {
        for o in &d.owners {
            indexes.push(IndexPairs {
                class: name_of(o.class_oid),
                index: String::from_utf8_lossy(&o.constraint.name).into(),
                bytes: by_class.get(&o.class_oid).copied().unwrap_or(0),
            });
        }
    }
    Ok(ResourceEstimate {
        indexes,
        per_def: counted.iter().map(|b| b.values().sum()).collect(),
        scan_seconds: 0.0,
        workers,
        physical_memory: 0,
    })
}

impl ResourceEstimate {
    pub fn temp_min(&self, mem: u64) -> u64 {
        self.per_def
            .iter()
            .map(|&t| spill_bound(t, mem))
            .max()
            .unwrap_or(0)
    }

    pub fn temp_max(&self, mem: u64) -> u64 {
        spill_bound(self.per_def.iter().sum(), mem)
    }

    /// Batches a temp limit cuts the indexes into; None below `temp_min`.
    pub fn batches(&self, limit: u64, mem: u64) -> Option<usize> {
        index_batches(&self.per_def, limit, mem)
            .ok()
            .map(|b| b.len())
    }

    /// Seconds `batches` add over one batch: one heap scan each.
    pub fn extra_seconds(&self, batches: usize) -> f64 {
        self.scan_seconds * batches.saturating_sub(1) as f64
    }

    /// What the copy engine needs: one I/O buffer per worker (the rest of the run spills under less).
    pub fn mem_min(&self) -> u64 {
        (self.workers * IO_BUF_BYTES) as u64
    }

    /// The smallest memory limit with no spill at full parallelism (`spill_bound` 0 for every index in one batch),
    /// at most the physical memory.
    pub fn mem_recommended(&self) -> u64 {
        let pairs: u64 = self.per_def.iter().sum();
        (2 * (pairs + WORKER_SHARE as u64))
            .max(self.mem_min())
            .min(self.physical_memory)
    }

    /// The memory limits the table shows: min, the run's own, recommended (sorted, distinct).
    pub fn memory_rows(&self, mem: u64) -> Vec<u64> {
        let mut rows = vec![self.mem_min(), mem, self.mem_recommended()];
        rows.sort_unstable();
        rows.dedup();
        rows
    }

    pub fn json(&self, mem: u64) -> String {
        let rows: Vec<String> = self
            .memory_rows(mem)
            .into_iter()
            .map(|m| {
                let (lo, hi) = (self.temp_min(m), self.temp_max(m));
                let b = self.batches(lo, m).unwrap_or(0);
                format!(
                    "{{\"mem_bytes\": {m}, \"temp_min_bytes\": {lo}, \"temp_max_bytes\": {hi}, \"batches_at_min\": {b}, \"extra_seconds_at_min\": {:.1}}}",
                    self.extra_seconds(b)
                )
            })
            .collect();
        let idx: Vec<String> = self
            .indexes
            .iter()
            .map(|i| {
                format!(
                    "{{\"class\": {:?}, \"index\": {:?}, \"pair_bytes\": {}}}",
                    i.class, i.index, i.bytes
                )
            })
            .collect();
        format!(
            "{{\"pair_bytes\": {}, \"scan_seconds\": {:.3}, \"workers\": {}, \"mem_min_bytes\": {}, \"mem_recommended_bytes\": {}, \"rows\": [{}], \"indexes\": [{}]}}",
            self.per_def.iter().sum::<u64>(),
            self.scan_seconds,
            self.workers,
            self.mem_min(),
            self.mem_recommended(),
            rows.join(", "),
            idx.join(", ")
        )
    }

    /// The table a person reads: one row per memory limit, temp min/max, batches and extra time at temp min.
    pub fn table(&self, mem: u64) -> String {
        let mib = |b: u64| b.div_ceil(1 << 20);
        let mut s = format!(
            "resources (index rebuild, --workers {}; counted in {:.1} s, one heap scan):\n  {:>12}  {:>14}  {:>14}  {:>16}  {:>18}\n",
            self.workers,
            self.scan_seconds,
            "--mem MiB",
            "temp min MiB",
            "temp max MiB",
            "batches at min",
            "extra s at min"
        );
        for m in self.memory_rows(mem) {
            let lo = self.temp_min(m);
            let b = self.batches(lo, m).unwrap_or(0);
            let tag = if m == mem {
                " (this run)"
            } else if m == self.mem_recommended() {
                " (recommended)"
            } else if m == self.mem_min() {
                " (min)"
            } else {
                ""
            };
            let _ = writeln!(
                s,
                "  {:>12}  {:>14}  {:>14}  {:>16}  {:>18.1}{tag}",
                mib(m),
                mib(lo),
                mib(self.temp_max(m)),
                b,
                self.extra_seconds(b)
            );
        }
        let _ = writeln!(
            s,
            "  --verify needs >= temp max: the verifier spills every index in one scan (docs/resources.md)"
        );
        s
    }
}
