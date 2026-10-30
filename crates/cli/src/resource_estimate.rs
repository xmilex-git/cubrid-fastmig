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
//! counting scan's time. mem_recommended spills less, not nothing; mem_min is the fixed buffers per worker (copy I/O
//! buffer, index build merge floor and spill buffer, `limits::mem_min`) — the keys spill under a smaller limit.
//! Temp disk the verifier needs comes after the build's spill is gone: a run needs max(build, verify), not the sum.
//! Source: workspace#526 (user requests: the sizes before the start, no disk full in the middle).

use std::collections::{HashMap, HashSet};
use std::io;
use std::time::Instant;

use crate::limits;
use fastmig_catalog::catalog_layout::generated_rows;
use fastmig_catalog::classify::ClassKind;
use fastmig_catalog::row_encode::encode_row;
use fastmig_catalog::same_version_catalog::{DevelopCatalog, deferred_rows};
use fastmig_catalog::source_catalog::SourceCatalog;
use fastmig_catalog::target_classes::{Origin, TargetClass as CatalogClass};
use fastmig_catalog::template_store::builtin;
use fastmig_index::IndexDef;
use fastmig_index::heap_scan::WORKER_SHARE;
use fastmig_index::pair_count::{PairStats, pair_bytes, record_pair_bytes};
use fastmig_index::rebuild::{TargetClass, class_constraints, plain_index_defs};
use fastmig_index::temp_disk::index_batches;
use fastmig_ondisk::heap_rewrite::{ClassReEncoder, ReEncode};
use fastmig_ondisk::volume_set::VolumeSet;
use fastmig_ondisk::{Oid, SourceVersion, VersionLine};
use fastmig_plan::version_pair::VersionPair;

use crate::local_datetime::now_datetime;

/// One index's pairs, by its owner class (a BTID shared by classes has one row per class).
pub struct IndexPairs {
    pub class: String,
    pub index: String,
    pub bytes: u64,
    pub stats: PairStats,
}

/// One index definition as the target space bound sees it.
pub struct DefFigures {
    pub class: String,
    pub index: String,
    pub unique: bool,
    pub stats: PairStats,
    /// Counted from heaps the source's B-trees index (a develop source, an 11.4 source's user classes); false for
    /// the system catalog rows an 11.4 run generates, which no source tree holds.
    pub from_heaps: bool,
}

pub struct ResourceEstimate {
    pub indexes: Vec<IndexPairs>,
    /// Pair bytes per index definition (owners summed): the input of the batching.
    pub per_def: Vec<u64>,
    /// Per index definition, owners merged: what the B-tree space bound needs (`target_space`).
    pub per_def_figures: Vec<DefFigures>,
    /// What the source's own B-trees hold, by (class, index) of their first owner (`verify::source_key_groups`):
    /// upper bounds of the keys and overflow OID chains of a definition counted from the source's heaps.
    pub source_groups: HashMap<(String, String), fastmig_verify::btree_walk::LeafCounts>,
    /// Seconds of the counting scan: one read of every indexed heap, the price of one more batch.
    pub scan_seconds: f64,
    pub workers: usize,
    pub physical_memory: u64,
    /// The run regenerates the system catalog (an 11.x or 10.2 source): its rows are built in memory.
    pub regenerates: bool,
    /// Spill bytes the verifier's source side takes (`verify::source_pair_bytes`), counted only for `--verify`.
    pub verify_source: Option<u64>,
    /// Heap pages of the source the verifier reads (its live row map), counted with `verify_source`.
    pub verify_heap_pages: Option<usize>,
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
            // the run deletes the deferred indexes' catalog rows before the build
            let (index_rows, key_rows) = deferred_rows(vols, &cat)?;
            let skip: HashSet<Oid> = index_rows.into_iter().chain(key_rows).collect();
            count(vols, &classes, Rows::Heaps(&HashMap::new(), &skip), workers)?
        }
        VersionLine::V10_2 | VersionLine::V11_4 => {
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
                        ClassReEncoder::new(&c.class, src.line, true)
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
            let mut e = count(
                vols,
                &tables(true),
                Rows::Heaps(&re_encoders, &HashSet::new()),
                workers,
            )?;
            let s = count(vols, &tables(false), Rows::Records(&records), workers)?;
            e.indexes.extend(s.indexes);
            e.per_def.extend(s.per_def);
            e.per_def_figures.extend(s.per_def_figures);
            e
        }
    };
    e.scan_seconds = started.elapsed().as_secs_f64();
    e.workers = workers;
    e.physical_memory = physical_memory;
    e.regenerates = pair.regenerates_catalog();
    Ok(e)
}

/// Where the records of the counted classes are.
enum Rows<'r> {
    /// In the classes' heaps, each class's records through its re-encoder if it has one, without the rows the
    /// run deletes before the build (a develop source's deferred indexes' catalog rows).
    Heaps(&'r HashMap<Oid, ClassReEncoder>, &'r HashSet<Oid>),
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
    let from_heaps = matches!(rows, Rows::Heaps(..));
    let counted = match rows {
        Rows::Heaps(re_encoders, skip) => pair_bytes(
            vols,
            &refs,
            workers,
            &|oid| re_encoders.get(&oid).map(|r| r as &dyn ReEncode),
            skip,
        ),
        Rows::Records(records) => record_pair_bytes(&refs, vols.io_pagesize(), &|oid| {
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
                bytes: by_class.get(&o.class_oid).map_or(0, |s| s.bytes),
                stats: by_class.get(&o.class_oid).copied().unwrap_or_default(),
            });
        }
    }
    Ok(ResourceEstimate {
        indexes,
        per_def: counted
            .iter()
            .map(|b| b.values().map(|s| s.bytes).sum())
            .collect(),
        per_def_figures: defs
            .iter()
            .zip(&counted)
            .map(|(d, b)| {
                let mut stats = PairStats::default();
                b.values().for_each(|c| stats.merge(c));
                DefFigures {
                    class: name_of(d.owners[0].class_oid),
                    index: String::from_utf8_lossy(&d.owners[0].constraint.name).into(),
                    unique: d.shape.unique,
                    stats,
                    from_heaps,
                }
            })
            .collect(),
        source_groups: HashMap::new(),
        scan_seconds: 0.0,
        workers,
        physical_memory: 0,
        regenerates: false,
        verify_source: None,
        verify_heap_pages: None,
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

    /// The smallest limit `--verify` takes: the verifier spills every index in one scan, on the target (temp max) and
    /// then on the source; None when the source side was not counted.
    pub fn verify_min(&self) -> Option<u64> {
        self.verify_source.map(|s| s.max(self.temp_max()))
    }

    /// The least `--mem` `--verify` runs in (`fastmig_verify::run::memory_min` on the source's heap pages, at least
    /// `mem_min`); None when the source side was not counted.
    pub fn verify_mem_min(&self) -> Option<u64> {
        self.verify_heap_pages
            .map(|p| (fastmig_verify::run::memory_min(p, self.workers) as u64).max(self.mem_min()))
    }

    /// Batches a temp limit cuts the indexes into; None below `temp_min`.
    pub fn batches(&self, limit: u64) -> Option<usize> {
        index_batches(&self.per_def, limit).ok().map(|b| b.len())
    }

    /// Seconds `batches` add over one batch: one heap scan each.
    pub fn extra_seconds(&self, batches: usize) -> f64 {
        self.scan_seconds * batches.saturating_sub(1) as f64
    }

    /// The smallest limit a run takes (`limits::mem_min`): fixed buffers per worker; the keys spill under it.
    pub fn mem_min(&self) -> u64 {
        limits::mem_min(self.workers)
    }

    /// The most RSS a run with `--mem` `mem` is expected to reach: the limit plus what stays outside it (docs/resources.md,
    /// "Memory outside --mem"), fitted from the measured runs as an upper envelope: 12 MiB of buffers, stacks and
    /// allocator, 18 MiB more when the catalog is regenerated, 32 MiB per GiB of pair bytes (run bookkeeping, sample
    /// keys, leaf summaries) and with `--verify` 24 MiB per GiB of the verifier's source side. Measured, not proven.
    pub fn rss_expected_max(&self, mem: u64, verify: bool) -> u64 {
        const MIB: f64 = (1u64 << 20) as f64;
        let gib = |b: u64| b as f64 / (1u64 << 30) as f64;
        let mut outside =
            12.0 + if self.regenerates { 18.0 } else { 0.0 } + 32.0 * gib(self.temp_max());
        if verify {
            outside += 24.0 * gib(self.verify_source.unwrap_or(self.temp_max()));
        }
        mem + (outside * MIB).ceil() as u64
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
                    "{{\"class\": {:?}, \"index\": {:?}, \"pair_bytes\": {}, \"pairs\": {}, \"max_key\": {}, \"ovf_key_pages\": {}}}",
                    i.class, i.index, i.bytes, i.stats.pairs, i.stats.max_key, i.stats.ovf_key_pages
                )
            })
            .collect();
        format!(
            "{{\"pair_bytes\": {hi}, \"scan_seconds\": {:.3}, \"workers\": {}, \"temp_min_bytes\": {lo}, \"temp_max_bytes\": {hi}, \"batches_at_min\": {b}, \"extra_seconds_at_min\": {:.1}, \"mem_min_bytes\": {}, \"mem_bytes\": {mem}, \"mem_recommended_bytes\": {}, \"verify_source_bytes\": {}, \"verify_min_bytes\": {}, \"verify_mem_min_bytes\": {}, \"rss_expected_max_bytes\": {}, \"rss_expected_max_verify_bytes\": {}, \"indexes\": [{}]}}",
            self.scan_seconds,
            self.workers,
            self.extra_seconds(b),
            self.mem_min(),
            self.mem_recommended(),
            opt(self.verify_source),
            opt(self.verify_min()),
            opt(self.verify_mem_min()),
            self.rss_expected_max(mem, false),
            self.rss_expected_max(mem, true),
            idx.join(", ")
        )
    }

    /// The table a person reads: temp min/max with the batches and extra time at min, then the memory limits.
    pub fn table(&self, mem: u64) -> String {
        let mib = |b: u64| b.div_ceil(1 << 20);
        let (lo, hi) = (self.temp_min(), self.temp_max());
        let b = self.batches(lo).unwrap_or(0);
        let verify = match self.verify_min() {
            Some(v) => format!(
                "--verify needs >= {} MiB: the verifier spills every index in one scan, on the target ({} MiB) then on the source ({} MiB) and --mem >= {} MiB (its live row map, merge floors and scan shares)",
                mib(v),
                mib(hi),
                mib(self.verify_source.unwrap_or(0)),
                mib(self.verify_mem_min().unwrap_or(0))
            ),
            None => "--verify needs >= temp max and the source side (counted by plan --verify; docs/resources.md)".into(),
        };
        format!(
            "resources (index rebuild, --workers {}; counted in {:.1} s, one heap scan):\n  temp disk: min {} MiB ({b} index batches, about {:.1} s more), max {} MiB (one batch)\n  {verify}\n  memory: min {} MiB, this run {} MiB, recommended {} MiB (fewer spills; temp does not depend on it)\n  expected max RSS at this run's --mem: {} MiB, {} MiB with --verify (--mem plus what stays outside it, measured: docs/resources.md)\n",
            self.workers,
            self.scan_seconds,
            mib(lo),
            self.extra_seconds(b),
            mib(hi),
            mib(self.mem_min()),
            mib(mem),
            mib(self.mem_recommended()),
            mib(self.rss_expected_max(mem, false)),
            mib(self.rss_expected_max(mem, true))
        )
    }
}

fn opt(v: Option<u64>) -> String {
    v.map_or_else(|| "null".into(), |b| b.to_string())
}
