//! Runs every check on one database and returns its defects, what could not be checked, counts and phase times.
//!
//! Input: an open [`Database`], the classes whose heap header statistics the migration measured, and the shared
//! worker / memory / spill limits. Output: [`DbResult`].
//! Order: check 4 (sectors; yields the tracked files) → every index tree opened → the live row map reserved → one
//! multi-class pass over every heap (checks 1 and the heap side of 2; the index builder's `scan_classes`,
//! workspace#503) → every in-memory run sorted → one pool on `--workers` threads, most leaf pages first: each plain
//! index merged with its heap side in leaf-range shards (check 2, `merge_shards`) and each foreign key against its
//! primary key → per index the checks across its shards and its root statistics → the root heap → the live row map
//! sealed → deferred indexes against it → catalog references and OBJECT values (check 3) → LOB files (check 5).
//! With a temp disk limit the plain indexes are cut into index batches (`temp_disk::index_batches` on their pair bytes,
//! counted first): the first batch's pass reads every heap and does every heap check, each later batch reads again
//! only the heaps of its indexes for their keys, and every batch's spill is gone before the next (workspace#553).
//! Source: G3 workspace#483 Q9 (one scan per class, all its indexes at once; merges per index; workers and --mem of
//! migrate), G5 workspace#486 (deferred indexes: structure + OID survival, key values "not checked"); workspace#509
//! (sorts, shards and foreign keys on the workers; measured on the big develop DB).

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::time::{Duration, Instant};

use fastmig_index::intermediate_merge::{MERGE_FLOOR, bound_open_runs};
use fastmig_index::job_pool::run_jobs;
use fastmig_index::memory_limit::MemoryGrant;
use fastmig_index::splitter::ShardBounds;
use fastmig_index::temp_disk::index_batches;
use fastmig_index::{BuildError, IndexRuns, MemoryLimit, Run, SpillDir, WorkerLimit};
use fastmig_ondisk::class_object::{Constraint, class_properties};
use fastmig_ondisk::file_table::FileTable;
use fastmig_ondisk::value::KeyDomain;
use fastmig_ondisk::{Btid, Oid, Vpid};
use fastmig_progress::Meter;

use crate::btree_walk::{IndexTree, LeafCounts, LeafPairs, ShardEnd, join_shards, leaf_counts};
use crate::catalog_refs::{CatalogInput, check_catalog};
use crate::database::Database;
use crate::defect::{Check, Defect, OidText};
use crate::foreign_key::check_fk;
use crate::heap_chain::check_chain;
use crate::heap_pass::{HeapScanJob, HeapTarget, IndexKeys, scan_heap, scan_heaps};
use crate::index_merge::{check_oids, merge_pairs};
use crate::live_row_map::LiveRowMap;
use crate::lob_files::check_lobs;
use crate::merge_shards::{plan_shards, shard_count};
use crate::object_refs::{ObjectRef, check_refs};
use crate::sector_owner::check_sectors;

pub struct Limits<'a> {
    pub workers: &'a WorkerLimit,
    pub memory: &'a MemoryLimit,
    pub spill: &'a SpillDir,
    /// Bytes spill files may hold at once (`--temp-disk`); None: every index in one heap pass (workspace#553).
    pub temp_disk: Option<u64>,
    /// The run's verifier phase (workspace#560): heap pages scanned, and in its second part ([`Meter::second`])
    /// index pairs merged, added per shard and per deferred index.
    pub progress: Option<Meter<'a>>,
}

#[derive(Default)]
pub struct DbResult {
    pub defects: Vec<Defect>,
    /// Indexes whose key values are not compared with the heap, and why (deferred indexes, workspace#486).
    pub not_checked: Vec<String>,
    pub heaps: usize,
    pub live_rows: usize,
    pub indexes: usize,
    pub index_pairs: u64,
    /// OBJECT values (check 3), LOB values (check 5) and foreign keys (check 2) compared.
    pub object_refs: usize,
    pub lobs: usize,
    pub foreign_keys: usize,
    /// Heap passes the keys took: 1, or the index batches of a temp disk limit (workspace#553).
    pub index_batches: usize,
    pub phases: Vec<(&'static str, Duration)>,
}

/// One index: the classes whose rows it holds and the constraint naming it in each.
struct IndexSpec {
    btid: Btid,
    owners: Vec<(usize, Constraint)>,
}

impl IndexSpec {
    /// A plain index: its keys come from the heap (a deferred index, function or filter, has none; workspace#486).
    fn is_plain(&self) -> bool {
        let c = &self.owners[0].1;
        !c.has_function && !c.has_filter
    }
}

fn phase<T>(
    phases: &mut Vec<(&'static str, Duration)>,
    name: &'static str,
    f: impl FnOnce() -> T,
) -> T {
    let t = Instant::now();
    let r = f();
    phases.push((name, t.elapsed()));
    r
}

fn tool_err(e: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(e.to_string())
}

/// The database's indexes: per class its constraints, one spec per BTID, and the trees opened — a plain one's key
/// domain drives the heap side; a deferred one (or an 11.4 DEDUPLICATE one) is checked without it.
struct Indexes<'a> {
    constraints: Vec<Result<Vec<Constraint>, String>>,
    specs: Vec<IndexSpec>,
    trees: BTreeMap<usize, IndexTree<'a>>,
    deferred: Vec<(usize, IndexTree<'a>)>,
    not_checked: Vec<String>,
    defects: Vec<Defect>,
}

fn open_indexes(db: &Database) -> Indexes<'_> {
    let constraints: Vec<Result<Vec<Constraint>, String>> = db
        .classes
        .iter()
        .map(|c| {
            if c.class.properties.is_empty() {
                Ok(Vec::new())
            } else {
                class_properties(&c.class.properties, db.line)
                    .map(|p| p.constraints)
                    .map_err(|e| e.to_string())
            }
        })
        .collect();
    let mut by_btid: BTreeMap<Btid, IndexSpec> = BTreeMap::new();
    for (i, cons) in constraints.iter().enumerate() {
        for k in cons.iter().flatten() {
            if let Some(b) = k.btid {
                let s = by_btid.entry(b).or_insert_with(|| IndexSpec {
                    btid: b,
                    owners: Vec::new(),
                });
                if !s.owners.iter().any(|(j, _)| *j == i) {
                    s.owners.push((i, k.clone()));
                }
            }
        }
    }
    let specs: Vec<IndexSpec> = by_btid.into_values().collect();
    let mut x = Indexes {
        constraints,
        specs: Vec::new(),
        trees: BTreeMap::new(),
        deferred: Vec::new(),
        not_checked: Vec::new(),
        defects: Vec::new(),
    };
    for (s, spec) in specs.iter().enumerate() {
        let (ci, c) = &spec.owners[0];
        let class = db.class_name(db.classes[*ci].oid);
        let name = String::from_utf8_lossy(&c.name).into_owned();
        match IndexTree::open(db, class, name.clone(), spec.btid) {
            Ok(t) => {
                // an 11.4 DEDUPLICATE tree is checked like any plain index: the heap side adds the hidden column
                // the constraint names (workspace#530); a root and a class object that disagree get one defect and
                // the structure check only, not a key-extract defect per row
                let dedup_mismatch = db.line == fastmig_ondisk::VersionLine::V11_4
                    && t.root.dedup_key_idx_114().is_some() != c.deduplicate_key.is_some();
                if dedup_mismatch {
                    x.defects.push(
                        Defect::new(Check::IndexHeap, "deduplicate-key")
                            .class(t.class.clone())
                            .index(name.clone())
                            .expected(format!(
                                "root deduplicate_key_idx as the class object ({:?})",
                                c.deduplicate_key
                            ))
                            .actual(format!("{:?}", t.root.dedup_key_idx_114())),
                    );
                }
                if !spec.is_plain() || dedup_mismatch {
                    if !spec.is_plain() {
                        x.not_checked.push(format!(
                            "{}.{name}: deferred index (function or filter)",
                            t.class
                        ));
                    }
                    x.deferred.push((s, t));
                } else {
                    x.trees.insert(s, t);
                }
            }
            Err(d) => x.defects.push(*d),
        }
    }
    x.specs = specs;
    x
}

/// Every class with a heap, by position in `db.classes`.
fn heap_classes(db: &Database) -> Vec<usize> {
    (0..db.classes.len())
        .filter(|&i| !db.classes[i].class.heap.is_null())
        .collect()
}

/// The heap scan jobs of `heap_classes` for the plain indexes in `batch` (spec positions): per class the batch's
/// indexes its rows feed and its job. `every_heap`: a class that feeds none is scanned too (the heap checks), else
/// left out. Returns the classes kept, their feeds and their jobs.
fn heap_jobs<'a>(
    db: &'a Database,
    specs: &'a [IndexSpec],
    trees: &'a BTreeMap<usize, IndexTree<'a>>,
    heap_classes: &[usize],
    batch: &BTreeSet<usize>,
    every_heap: bool,
) -> (Vec<usize>, Vec<Vec<usize>>, Vec<HeapScanJob<'a>>) {
    let mut kept = Vec::with_capacity(heap_classes.len());
    let mut feeds: Vec<Vec<usize>> = Vec::with_capacity(heap_classes.len());
    let mut jobs: Vec<HeapScanJob> = Vec::with_capacity(heap_classes.len());
    for &ci in heap_classes {
        let mine: Vec<usize> = batch
            .iter()
            .copied()
            .filter(|s| specs[*s].owners.iter().any(|(j, _)| *j == ci))
            .collect();
        if mine.is_empty() && !every_heap {
            continue;
        }
        let sc = &db.classes[ci];
        let indexes = mine
            .iter()
            .map(|s| IndexKeys {
                constraint: &specs[*s].owners.iter().find(|(j, _)| *j == ci).unwrap().1,
                kd: &trees[s].kd,
            })
            .collect();
        kept.push(ci);
        feeds.push(mine);
        jobs.push(HeapScanJob {
            target: HeapTarget {
                name: db.class_name(sc.oid),
                class_oid: sc.oid,
                class: Some(&sc.class),
                hfid: sc.class.heap,
            },
            indexes,
        });
    }
    (kept, feeds, jobs)
}

/// Spill bytes of each plain index's pairs on `db` (by spec position, `trees` order): the verifier's own reader over
/// the heaps that feed them, nothing kept (`heap_pass::pair_bytes`).
fn index_pair_bytes(
    db: &Database,
    specs: &[IndexSpec],
    trees: &BTreeMap<usize, IndexTree>,
    workers: usize,
) -> std::io::Result<BTreeMap<usize, u64>> {
    let all: BTreeSet<usize> = trees.keys().copied().collect();
    let (_, feeds, jobs) = heap_jobs(db, specs, trees, &heap_classes(db), &all, false);
    let mut out: BTreeMap<usize, u64> = trees.keys().map(|&s| (s, 0)).collect();
    for (mine, bytes) in feeds
        .iter()
        .zip(crate::heap_pass::pair_bytes(db, &jobs, workers)?)
    {
        for (s, b) in mine.iter().zip(bytes) {
            *out.get_mut(s).unwrap() += b;
        }
    }
    Ok(out)
}

/// The spill bytes of each plain index of this verifier on `db` when every pair spills, and the heap pages its live
/// row map is reserved for (workspace#526, #553: the temp disk `--verify` needs on the source, its least the largest
/// index, its most the sum).
pub fn heap_pair_bytes(db: &Database, workers: usize) -> std::io::Result<(Vec<u64>, usize)> {
    let x = open_indexes(db);
    let bytes = index_pair_bytes(db, &x.specs, &x.trees, workers)?;
    Ok((
        bytes.into_values().collect(),
        heap_pages(db, &heap_classes(db)),
    ))
}

/// User pages of every heap the pass reads, the root heap included: what the live row map is reserved for.
fn heap_pages(db: &Database, heap_classes: &[usize]) -> usize {
    heap_classes
        .iter()
        .map(|&i| db.classes[i].class.heap)
        .chain(std::iter::once(db.boot.rootclass_hfid))
        .map(|h| FileTable::read(&db.vols, h.vfid).map_or(0, |ft| ft.user_pages().len()))
        .sum()
}

/// The plain indexes cut into batches: one with no temp disk limit, else `temp_disk::index_batches` on their pair
/// bytes (workspace#553). Err: the limit is below the largest index.
fn batches(
    db: &Database,
    specs: &[IndexSpec],
    trees: &BTreeMap<usize, IndexTree>,
    lim: &Limits,
) -> std::io::Result<Vec<BTreeSet<usize>>> {
    // no plain index: still one batch, whose pass does the heap checks
    let Some(limit) = lim.temp_disk.filter(|_| !trees.is_empty()) else {
        return Ok(vec![trees.keys().copied().collect()]);
    };
    let (at, bytes): (Vec<usize>, Vec<u64>) =
        index_pair_bytes(db, specs, trees, lim.workers.limit())?
            .into_iter()
            .unzip();
    let cut = index_batches(&bytes, limit).map_err(|min| {
        tool_err(format!(
            "temp disk: {limit} B is below {min} B, the pair bytes of the verifier's largest index"
        ))
    })?;
    Ok(cut
        .into_iter()
        .map(|b| b.into_iter().map(|i| at[i]).collect())
        .collect())
}

pub fn verify_database(
    db: &Database,
    measured: &BTreeSet<Oid>,
    lim: &Limits,
) -> std::io::Result<DbResult> {
    let mut r = DbResult::default();
    let mut phases = Vec::new();
    let (files, d4) = phase(&mut phases, "sectors", || check_sectors(db))?;
    r.defects.extend(d4);

    let Indexes {
        constraints,
        specs,
        mut trees,
        deferred,
        not_checked,
        defects,
    } = open_indexes(db);
    r.indexes = specs.len();
    r.not_checked.extend(not_checked);
    r.defects.extend(defects);
    // a tree's own defects (from open) are reported once, whatever its shards find
    for t in trees.values_mut() {
        r.defects.extend(std::mem::take(&mut t.defects));
    }
    let trees = trees;
    let batches = phase(&mut phases, "pair count", || {
        batches(db, &specs, &trees, lim)
    })?;
    r.index_batches = batches.len();
    let heap_classes = heap_classes(db);
    // the live row map is reserved before the scan, from the heaps' page counts (workspace#509)
    let mut live = LiveRowMap::reserve(lim.memory, heap_pages(db, &heap_classes))?;
    let mut refs: Vec<ObjectRef> = Vec::new();
    let fks = foreign_key_jobs(db, &constraints);
    r.foreign_keys = fks.len();
    let (mut t_heaps, mut t_sort, mut t_merge) = (Duration::ZERO, Duration::ZERO, Duration::ZERO);
    for (k, batch) in batches.iter().enumerate() {
        // the first batch's pass reads every heap for every heap check; a later one only its indexes' heaps, for keys
        let first = k == 0;
        let t = Instant::now();
        let (kept, feeds, jobs) = heap_jobs(db, &specs, &trees, &heap_classes, batch, first);
        // merge memory (fastmig_index::intermediate_merge): a floor per worker held from before the scan, which
        // cannot take it, and after the sorts all the limit has free; each check-2 job merges within one worker's part
        let floor = lim.memory.try_acquire(lim.workers.limit() * MERGE_FLOOR);
        let mut runs: BTreeMap<usize, IndexRuns> = BTreeMap::new();
        let mut nulls: BTreeMap<usize, u64> = BTreeMap::new();
        let passes = scan_heaps(db, &jobs, lim.workers, lim.memory, lim.spill, lim.progress)
            .map_err(build_err)?;
        for (((job, &ci), mine), p) in jobs.iter().zip(&kept).zip(&feeds).zip(passes) {
            if first {
                let sc = &db.classes[ci];
                let t = &job.target;
                r.heaps += 1;
                r.defects
                    .extend(check_chain(t, &p, measured.contains(&sc.oid)));
                r.defects.extend(check_lobs(
                    &p.lobs,
                    db.lob_base.as_deref(),
                    db.line,
                    &t.name,
                ));
                r.lobs += p.lobs.len();
                live.add(sc.oid, p.live)?;
                refs.extend(p.refs);
                r.defects.extend(p.defects);
            } else {
                // the first pass reported the heap's own defects; a key defect is of this batch's indexes only
                r.defects.extend(
                    p.defects
                        .into_iter()
                        .filter(|d| d.check == Check::IndexHeap),
                );
            }
            for ((s, run), n) in mine.iter().zip(p.runs.runs).zip(p.runs.nulls) {
                runs.entry(*s).or_default().append(run);
                *nulls.entry(*s).or_default() += n;
            }
        }
        drop((jobs, feeds));
        t_heaps += t.elapsed();
        let index_runs: Vec<(usize, IndexRuns, u64)> = batch
            .iter()
            .map(|s| {
                (
                    *s,
                    runs.remove(s).unwrap_or_default(),
                    nulls.get(s).copied().unwrap_or(0),
                )
            })
            .collect();
        // foreign keys read only trees: they go with the first batch's pool
        let fk_jobs = if first { fks.as_slice() } else { &[] };
        check_batch(
            &trees,
            index_runs,
            floor,
            fk_jobs,
            lim,
            &mut r,
            &mut t_sort,
            &mut t_merge,
        )?;
    }
    phases.push(("heap pass", t_heaps));
    phases.push(("run sort", t_sort));
    phases.push(("index merge and foreign keys", t_merge));

    phase(&mut phases, "root heap", || -> std::io::Result<()> {
        let t = HeapTarget {
            name: "<root>".into(),
            class_oid: db.boot.rootclass_oid,
            class: None,
            hfid: db.boot.rootclass_hfid,
        };
        let p = scan_heap(
            db,
            t.clone(),
            Vec::new(),
            lim.workers,
            lim.memory,
            lim.spill,
            lim.progress,
        )
        .map_err(build_err)?;
        r.heaps += 1;
        r.defects.extend(check_chain(&t, &p, false));
        live.add(db.boot.rootclass_oid, p.live)?;
        r.defects.extend(p.defects);
        Ok(())
    })?;
    let twice = phase(&mut phases, "live rows", || live.seal());
    for (oid, a, b) in twice {
        r.defects.push(
            Defect::new(Check::HeapChain, "row-in-two-heaps")
                .object(OidText(oid))
                .actual(format!("{} and {}", db.class_name(a), db.class_name(b))),
        );
    }
    r.live_rows = live.len();

    phase(&mut phases, "deferred indexes", || {
        for (s, mut t) in deferred {
            r.defects.extend(std::mem::take(&mut t.defects));
            let owners: BTreeSet<Oid> = specs[s]
                .owners
                .iter()
                .map(|(j, _)| db.classes[*j].oid)
                .collect();
            let mut lp = LeafPairs::new(&t);
            let mut d = check_oids(&mut lp, &live, &|c| owners.contains(&c));
            r.index_pairs += lp.counts.live_objects as u64;
            if let Some(m) = lim.progress.map(Meter::second) {
                m.add(lp.counts.live_objects as u64);
            }
            d.extend(lp.finish(None));
            r.defects.extend(d);
        }
    });

    phase(&mut phases, "catalog", || {
        r.defects.extend(check_catalog(&CatalogInput {
            db,
            files: &files,
            live: &live,
            constraints: &constraints,
            refs: &refs,
        }));
        let accepts = |want: Oid, got: Oid| {
            want == got
                || db
                    .by_oid
                    .get(&got)
                    .is_some_and(|&j| db.classes[j].class.super_classes.contains(&want))
        };
        r.defects
            .extend(check_refs(&refs, &live, &|c| db.class_name(c), &accepts));
        r.object_refs = refs.len();
    });
    r.phases = phases;
    Ok(r)
}

/// One batch's indexes after its heap pass: every in-memory run sorted, the spilled ones bounded to the merge memory,
/// then every shard merged with its heap side (check 2) and `fks` against their primary keys in one pool, then per
/// index the checks across its shards and its root statistics. The runs, their spill space and the merge memory go
/// back when it returns.
#[allow(clippy::too_many_arguments)]
fn check_batch(
    trees: &BTreeMap<usize, IndexTree>,
    mut index_runs: Vec<(usize, IndexRuns, u64)>,
    floor: MemoryGrant,
    fks: &[(IndexTree, Vec<IndexTree>)],
    lim: &Limits,
    r: &mut DbResult,
    t_sort: &mut Duration,
    t_merge: &mut Duration,
) -> std::io::Result<()> {
    let t = Instant::now();
    {
        // every in-memory run sorted on --workers threads, largest first, not on its index's merge thread
        // (workspace#509)
        let mut sorts: Vec<(&mut Run, &KeyDomain)> = index_runs
            .iter_mut()
            .flat_map(|(s, run, _)| {
                let kd = &trees[s].kd;
                run.mem.iter_mut().map(move |m| (m, kd))
            })
            .collect();
        sorts.sort_by_key(|(m, _)| std::cmp::Reverse(m.len()));
        run_jobs(lim.workers.limit(), sorts, |(m, kd)| m.sort(kd));
    }
    let free = lim.memory.try_acquire(lim.memory.limit());
    let merge_budget = (floor.bytes + free.bytes) / lim.workers.limit().max(1);
    let bounded = run_jobs(
        lim.workers.limit(),
        index_runs.iter_mut().collect(),
        |(s, run, _): &mut (usize, IndexRuns, u64)| {
            bound_open_runs(run, &trees[s].kd, lim.spill, merge_budget)
        },
    );
    bounded.into_iter().collect::<std::io::Result<()>>()?;
    let plans = index_runs
        .iter()
        .map(|(s, run, _)| {
            let t = &trees[s];
            plan_shards(t, run, shard_count(t.leaves.len(), lim.workers.limit()))
        })
        .collect::<std::io::Result<Vec<_>>>()?;
    *t_sort += t.elapsed();

    let t = Instant::now();
    // every shard of every plain index merged with its heap side, and every foreign key against its primary key, in
    // one pool on --workers threads, the most leaf pages first (workspace#503, #509)
    let mut jobs: Vec<TreeJob> = Vec::new();
    for (i, ((s, runs, _), (ranges, bounds))) in index_runs.iter().zip(&plans).enumerate() {
        for (k, leaves) in ranges.iter().enumerate() {
            jobs.push(TreeJob::Shard {
                index: i,
                shard: k,
                tree: &trees[s],
                runs,
                slice: bounds.as_ref().map(|b| (b, k)),
                leaves: leaves.clone(),
            });
        }
    }
    jobs.extend(fks.iter().map(|(fk, pks)| TreeJob::ForeignKey { fk, pks }));
    jobs.sort_by_key(|j| std::cmp::Reverse(j.leaf_pages()));
    let results = run_jobs(lim.workers.limit(), jobs, |j| match j {
        TreeJob::Shard {
            index,
            shard,
            tree,
            runs,
            slice,
            leaves,
        } => {
            let mut lp = LeafPairs::shard(tree, leaves);
            let mut d = merge_pairs(runs, slice, &mut lp)?;
            let (ld, counts, end) = lp.into_shard();
            if let Some(m) = lim.progress.map(Meter::second) {
                m.add(counts.live_objects as u64);
            }
            d.extend(ld);
            Ok::<_, std::io::Error>((d, Some((index, shard, counts, end))))
        }
        TreeJob::ForeignKey { fk, pks } => Ok((check_fk(fk, pks), None)),
    });
    let mut shards: Vec<Vec<(usize, LeafCounts, ShardEnd)>> =
        index_runs.iter().map(|_| Vec::new()).collect();
    for res in results {
        let (d, shard) = res?;
        r.defects.extend(d);
        if let Some((i, k, counts, end)) = shard {
            shards[i].push((k, counts, end));
        }
    }
    // per index: the checks across its shards' edges, then its root statistics over all of them
    for ((s, _, nulls), mut parts) in index_runs.iter().zip(shards) {
        parts.sort_by_key(|p| p.0);
        let t = &trees[s];
        let mut sum = LeafCounts::default();
        for (_, c, _) in &parts {
            sum += *c;
        }
        let ends: Vec<ShardEnd> = parts.into_iter().map(|p| p.2).collect();
        r.defects.extend(join_shards(t, &ends));
        r.defects.extend(t.check_stats(&sum, Some(*nulls)));
        r.index_pairs += sum.live_objects as u64;
    }
    *t_merge += t.elapsed();
    // the keys are checked: their runs, spill space and the merge memory go back before the next batch or the root
    // heap's own scan
    drop((plans, index_runs, free, floor));
    Ok(())
}

fn build_err(e: BuildError) -> std::io::Error {
    tool_err(e)
}

/// Every foreign key's tree with its referenced primary key trees (with the partitions' primary keys of a
/// partitioned class), opened for `check_fk`.
fn foreign_key_jobs<'a>(
    db: &'a Database,
    constraints: &[Result<Vec<Constraint>, String>],
) -> Vec<(IndexTree<'a>, Vec<IndexTree<'a>>)> {
    let mut out = Vec::new();
    for (i, cons) in constraints.iter().enumerate() {
        for k in cons.iter().flatten() {
            let (Some(fk), Some(btid)) = (&k.foreign_key, k.btid) else {
                continue;
            };
            let Some(&ri) = db.by_oid.get(&fk.ref_class) else {
                continue; // check 3 reports the missing class
            };
            let open = |ci: usize, c: &Constraint, b: Btid| {
                IndexTree::open(
                    db,
                    db.class_name(db.classes[ci].oid),
                    String::from_utf8_lossy(&c.name).into_owned(),
                    b,
                )
                .ok()
            };
            // the referenced index by BTID in the class, by name in its partitions (they carry the same constraints)
            let Some(ref_name) = constraints[ri]
                .as_ref()
                .ok()
                .and_then(|rc| {
                    rc.iter()
                        .find(|c| c.btid.is_some() && c.btid == fk.ref_pk_btid)
                })
                .map(|c| c.name.clone())
            else {
                continue; // check 3 reports fk-ref
            };
            let mut pk_classes = vec![ri];
            pk_classes.extend(
                db.classes[ri]
                    .class
                    .sub_classes
                    .iter()
                    .filter_map(|o| db.by_oid.get(o).copied()),
            );
            let pks: Vec<IndexTree> = pk_classes
                .iter()
                .filter_map(|&ci| {
                    let cons = constraints[ci].as_ref().ok()?;
                    let pk = cons
                        .iter()
                        .find(|c| c.name == ref_name && c.btid.is_some())?;
                    open(ci, pk, pk.btid?)
                })
                .collect();
            let Some(fk_tree) = open(i, k, btid) else {
                continue; // check 2 reports the unreadable index
            };
            // check 2 walks these trees too; the FK job adds only the key comparison
            out.push((fk_tree, pks));
        }
    }
    out
}

/// One job of the tree pool: a shard of an index merged with its heap side (check 2), or a foreign key against its
/// primary key.
enum TreeJob<'j, 'a, 'm> {
    Shard {
        /// The index (position in the pool's index list) and the shard's place in its leaf order.
        index: usize,
        shard: usize,
        tree: &'j IndexTree<'a>,
        runs: &'j IndexRuns<'m>,
        slice: Option<(&'j ShardBounds, usize)>,
        leaves: Range<usize>,
    },
    ForeignKey {
        fk: &'j IndexTree<'a>,
        pks: &'j [IndexTree<'a>],
    },
}

impl TreeJob<'_, '_, '_> {
    /// The leaf pages the job walks: the one cost unit of both kinds, for largest-first order.
    fn leaf_pages(&self) -> usize {
        match self {
            TreeJob::Shard { leaves, .. } => leaves.len(),
            TreeJob::ForeignKey { fk, pks } => {
                fk.leaves.len() + pks.iter().map(|t| t.leaves.len()).sum::<usize>()
            }
        }
    }
}

/// The least `--mem` the verifier runs `workers` workers in over `heap_pages` heap pages: the live row map's
/// reservation, the merge floor of every worker, and one heap scan share per worker (workspace#526).
pub fn memory_min(heap_pages: usize, workers: usize) -> usize {
    heap_pages * crate::live_row_map::ESTIMATE_PAGE_BYTES
        + workers * (MERGE_FLOOR + fastmig_index::heap_scan::SCAN_MIN)
}

/// Per plain index of `db`, its class and name (first owner) and what its leaves hold (records, wide records,
/// objects past a record): the leaves of every tree in one queue, a chunk per job (workspace#526: upper bounds of
/// the keys and overflow OID chains a rebuild of the live rows makes, the source's index holding every live row's
/// key; a big tree is no longer one worker's).
pub fn index_key_groups(db: &Database, workers: usize) -> Vec<(String, String, LeafCounts)> {
    const LEAVES_PER_JOB: usize = 64;
    let x = open_indexes(db);
    let trees: Vec<&IndexTree> = x.trees.values().collect();
    let mut units: Vec<(usize, Vec<Vpid>)> = Vec::new();
    for (k, t) in trees.iter().enumerate() {
        for chunk in t.leaves.chunks(LEAVES_PER_JOB) {
            units.push((k, chunk.iter().map(|(v, _)| *v).collect()));
        }
    }
    let mut out: Vec<LeafCounts> = vec![LeafCounts::default(); trees.len()];
    for (k, c) in run_jobs(workers, units, |(k, leaves)| {
        let mut c = LeafCounts::default();
        for v in leaves {
            let l = leaf_counts(trees[k], v);
            c.records += l.records;
            c.objects += l.objects;
            c.wide_records += l.wide_records;
            c.objects_past_record += l.objects_past_record;
        }
        (k, c)
    }) {
        out[k].records += c.records;
        out[k].objects += c.objects;
        out[k].wide_records += c.wide_records;
        out[k].objects_past_record += c.objects_past_record;
    }
    trees
        .iter()
        .zip(out)
        .map(|(t, c)| (t.class.clone(), t.name.clone(), c))
        .collect()
}
