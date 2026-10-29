//! The plan's resource table (workspace#526): the temp disk (spill) and memory the index rebuild needs, so the user
//! picks `--temp-disk` and `--mem` before anything is written.
//!
//! Input: the source volumes, the version pair, the worker limit. Output: [`ResourceEstimate`] — per index the bytes
//! its (key, OID) pairs take as spill records (counted by reading the source heaps with the build's own key
//! extractor, `fastmig_index::pair_count`; an 11.4 source's rows re-encoded first, its system catalog rows
//! generated as the run writes them), the counting scan's seconds, and from those: temp min/max, the index
//! batches a temp limit needs and their extra time, and the memory rows.
//! Bounds (docs/resources.md): a scan spills at most the pair bytes of its indexes, whatever `--mem` is: temp_min
//! = the largest index's, temp_max = all of them in one batch; a batch is one more heap scan, priced at the
//! counting scan's time. mem_recommended spills less, not nothing; mem_min is what the copy engine needs (one I/O
//! buffer per worker) — the rest of the run degrades to spill under a smaller limit.
//! Temp disk the verifier needs comes after the build's spill is gone: a run needs max(build, verify), not the sum.
//! Source: workspace#526 (user requests: the sizes before the start, no disk full in the middle).

use std::collections::HashMap;
use std::io;
use std::time::Instant;

use fastmig_catalog::catalog_layout::generated_rows;
use fastmig_catalog::classify::ClassKind;
use fastmig_catalog::row_encode::encode_row;
use fastmig_catalog::same_version_catalog::DevelopCatalog;
use fastmig_catalog::source_catalog::SourceCatalog;
use fastmig_catalog::target_classes::{Origin, TargetClass as CatalogClass};
use fastmig_catalog::template_store::builtin;
use fastmig_copy::volume_copy::IO_BUF_BYTES;
use fastmig_index::IndexDef;
use fastmig_index::heap_scan::WORKER_SHARE;
use fastmig_index::pair_count::{pair_bytes, record_pair_bytes};
use fastmig_index::rebuild::{TargetClass, class_constraints, plain_index_defs};
use fastmig_index::temp_disk::index_batches;
use fastmig_ondisk::heap_rewrite::{ClassReEncoder, ReEncode};
use fastmig_ondisk::volume_set::VolumeSet;
use fastmig_ondisk::{Oid, SourceVersion, VersionLine};
use fastmig_plan::version_pair::VersionPair;

use crate::catalog_regeneration::now_datetime;

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

/// The estimate of a source: a develop source's classes are counted as they are. An 11.4 source is counted as the
/// target will hold it, from the regeneration's first pass (`generated_rows`, nothing written): user classes through
/// the class objects the run writes and, for a re-encoded class, each row through its re-encoder; system classes
/// from the catalog rows the run generates, encoded as it encodes them (`encode_row`).
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
            count(vols, &classes, Rows::Heaps(&HashMap::new()), workers)?
        }
        VersionLine::V11_4 => {
            let src = SourceCatalog::read(vols, source)?;
            let template = builtin(VersionLine::Develop, src.charset).ok_or_else(|| {
                invalid(format!("no catalog template for charset {}", src.charset))
            })?;
            // the class objects and catalog rows the run writes, without writing them
            let g = generated_rows(&src, &template, &now_datetime()).map_err(invalid)?;
            let mut re_encoders = HashMap::new();
            for (c, p) in src.classes.iter().zip(&src.plans) {
                if p.kind == ClassKind::User && p.re_encoded {
                    // user classes are MVCC classes (page_treatment); LOB locators are not keys
                    re_encoders.insert(
                        c.oid,
                        ClassReEncoder::new(&c.class, true)
                            .map_err(|e| invalid(format!("{}: {e}", p.name)))?,
                    );
                }
            }
            let by_name: HashMap<&[u8], &CatalogClass> = g
                .targets
                .iter()
                .map(|t| (t.class.name.as_slice(), t))
                .collect();
            let mut records: HashMap<Oid, Vec<(Oid, Vec<u8>)>> = HashMap::new();
            for (table, row) in &g.rows {
                let t = by_name
                    .get(table.name().as_bytes())
                    .ok_or_else(|| invalid(format!("no target class {table}")))?;
                let rec =
                    encode_row(&t.class, row).map_err(|e| invalid(format!("{table}: {e}")))?;
                records.entry(t.oid).or_default().push((row.oid, rec));
            }
            let tables = |user: bool| -> Vec<TargetClass> {
                g.targets
                    .iter()
                    .filter(|t| !t.class.heap.is_null() && (t.origin == Origin::User) == user)
                    .map(|t| TargetClass {
                        oid: t.oid,
                        class: &t.class,
                        hfid: t.class.heap,
                    })
                    .collect()
            };
            let mut e = count(vols, &tables(true), Rows::Heaps(&re_encoders), workers)?;
            let s = count(vols, &tables(false), Rows::Records(&records), workers)?;
            e.indexes.extend(s.indexes);
            e.per_def.extend(s.per_def);
            e
        }
    };
    e.scan_seconds = started.elapsed().as_secs_f64();
    e.workers = workers;
    e.physical_memory = physical_memory;
    Ok(e)
}

/// Where the records of the counted classes are.
enum Rows<'r> {
    /// In the classes' heaps, each class's records through its re-encoder if it has one.
    Heaps(&'r HashMap<Oid, ClassReEncoder>),
    /// Not on any page yet (the catalog rows an 11.4 source's run generates): develop records by class OID.
    Records(&'r HashMap<Oid, Vec<(Oid, Vec<u8>)>>),
}

/// Counts the pairs of every plain index of `classes`.
fn count(
    vols: &VolumeSet,
    classes: &[TargetClass],
    rows: Rows,
    workers: usize,
) -> io::Result<ResourceEstimate> {
    let cons = class_constraints(classes).map_err(invalid)?;
    let charset = vols.volume(0)?.header.db_charset as u8;
    let (defs, _) =
        plain_index_defs(classes, &cons, vols.io_pagesize(), charset).map_err(invalid)?;
    let refs: Vec<&IndexDef> = defs.iter().collect();
    let counted = match rows {
        Rows::Heaps(re_encoders) => pair_bytes(vols, &refs, workers, &|oid| {
            re_encoders.get(&oid).map(|r| r as &dyn ReEncode)
        }),
        Rows::Records(records) => record_pair_bytes(&refs, &|oid| {
            records.get(&oid).map_or(&[], |r| r.as_slice())
        }),
    }
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
    /// The largest index alone: the smallest limit that lets the run finish.
    pub fn temp_min(&self) -> u64 {
        self.per_def.iter().copied().max().unwrap_or(0)
    }

    /// Every index in one scan: what a run spills at most.
    pub fn temp_max(&self) -> u64 {
        self.per_def.iter().sum()
    }

    /// Batches a temp limit cuts the indexes into; None below `temp_min`.
    pub fn batches(&self, limit: u64) -> Option<usize> {
        index_batches(&self.per_def, limit).ok().map(|b| b.len())
    }

    /// Seconds `batches` add over one batch: one heap scan each.
    pub fn extra_seconds(&self, batches: usize) -> f64 {
        self.scan_seconds * batches.saturating_sub(1) as f64
    }

    /// What the copy engine needs: one I/O buffer per worker (the rest of the run spills under less).
    pub fn mem_min(&self) -> u64 {
        (self.workers * IO_BUF_BYTES) as u64
    }

    /// Room for every pair (as spill bytes) in finished runs plus the workers' shares, at most the physical memory:
    /// fewer spills, not none (an in-memory run takes more than its spill bytes); the temp limit does not depend on
    /// it.
    pub fn mem_recommended(&self) -> u64 {
        (2 * (self.temp_max() + WORKER_SHARE as u64))
            .max(self.mem_min())
            .min(self.physical_memory)
    }

    pub fn json(&self, mem: u64) -> String {
        let (lo, hi) = (self.temp_min(), self.temp_max());
        let b = self.batches(lo).unwrap_or(0);
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
            "{{\"pair_bytes\": {hi}, \"scan_seconds\": {:.3}, \"workers\": {}, \"temp_min_bytes\": {lo}, \"temp_max_bytes\": {hi}, \"batches_at_min\": {b}, \"extra_seconds_at_min\": {:.1}, \"mem_min_bytes\": {}, \"mem_bytes\": {mem}, \"mem_recommended_bytes\": {}, \"indexes\": [{}]}}",
            self.scan_seconds,
            self.workers,
            self.extra_seconds(b),
            self.mem_min(),
            self.mem_recommended(),
            idx.join(", ")
        )
    }

    /// The table a person reads: temp min/max with the batches and extra time at min, then the memory limits.
    pub fn table(&self, mem: u64) -> String {
        let mib = |b: u64| b.div_ceil(1 << 20);
        let (lo, hi) = (self.temp_min(), self.temp_max());
        let b = self.batches(lo).unwrap_or(0);
        format!(
            "resources (index rebuild, --workers {}; counted in {:.1} s, one heap scan):\n  temp disk: min {} MiB ({b} index batches, about {:.1} s more), max {} MiB (one batch)\n  --verify needs >= temp max: the verifier spills every index in one scan (docs/resources.md)\n  memory: min {} MiB, this run {} MiB, recommended {} MiB (fewer spills; temp does not depend on it)\n",
            self.workers,
            self.scan_seconds,
            mib(lo),
            self.extra_seconds(b),
            mib(hi),
            mib(self.mem_min()),
            mib(mem),
            mib(self.mem_recommended())
        )
    }
}
