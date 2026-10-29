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
//! Source: G3 workspace#483 Q9 (one scan per class, all its indexes at once; merges per index; workers and --mem of
//! migrate), G5 workspace#486 (deferred indexes: structure + OID survival, key values "not checked"); workspace#509
//! (sorts, shards and foreign keys on the workers; measured on the big develop DB).

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::time::{Duration, Instant};

use fastmig_index::job_pool::run_jobs;
use fastmig_index::splitter::ShardBounds;
use fastmig_index::{BuildError, IndexRuns, MemoryLimit, Run, SpillDir, WorkerLimit};
use fastmig_ondisk::class_object::{Constraint, class_properties};
use fastmig_ondisk::file_table::FileTable;
use fastmig_ondisk::value::KeyDomain;
use fastmig_ondisk::{Btid, Oid};

use crate::btree_walk::{IndexTree, LeafCounts, LeafPairs, ShardEnd, join_shards};
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

pub fn verify_database(
    db: &Database,
    measured: &BTreeSet<Oid>,
    lim: &Limits,
) -> std::io::Result<DbResult> {
    let mut r = DbResult::default();
    let mut phases = Vec::new();
    let (files, d4) = phase(&mut phases, "sectors", || check_sectors(db))?;
    r.defects.extend(d4);

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
    r.indexes = specs.len();

    let mut refs: Vec<ObjectRef> = Vec::new();
    let mut deferred: Vec<(usize, IndexTree)> = Vec::new();
    let t_heaps = Instant::now();
    // every index tree first: a plain one's key domain drives the heap side
    let mut trees: BTreeMap<usize, IndexTree> = BTreeMap::new();
    for (s, spec) in specs.iter().enumerate() {
        let (ci, c) = &spec.owners[0];
        let class = db.class_name(db.classes[*ci].oid);
        let name = String::from_utf8_lossy(&c.name).into_owned();
        match IndexTree::open(db, class, name.clone(), spec.btid) {
            Ok(t) => {
                let dedup = t.root.dedup_key_idx_114().is_some()
                    && db.line == fastmig_ondisk::VersionLine::V11_4;
                if !spec.is_plain() || dedup {
                    r.not_checked.push(format!(
                        "{}.{name}: {}",
                        t.class,
                        if dedup {
                            "11.4 DEDUPLICATE index (hidden key column)"
                        } else {
                            "deferred index (function or filter)"
                        }
                    ));
                    deferred.push((s, t));
                } else {
                    trees.insert(s, t);
                }
            }
            Err(d) => r.defects.push(*d),
        }
    }
    // one multi-class scan over every heap (workspace#503), then the class checks in class order
    let heap_classes: Vec<usize> = (0..db.classes.len())
        .filter(|&i| !db.classes[i].class.heap.is_null())
        .collect();
    // the live row map is reserved before the scan, from the heaps' page counts (workspace#509)
    let heap_pages: usize = heap_classes
        .iter()
        .map(|&i| db.classes[i].class.heap)
        .chain(std::iter::once(db.boot.rootclass_hfid))
        .map(|h| FileTable::read(&db.vols, h.vfid).map_or(0, |ft| ft.user_pages().len()))
        .sum();
    let mut live = LiveRowMap::reserve(lim.memory, heap_pages)?;
    let mut runs: BTreeMap<usize, IndexRuns> = BTreeMap::new();
    let mut nulls: BTreeMap<usize, u64> = BTreeMap::new();
    {
        let mut feeds: Vec<Vec<usize>> = Vec::with_capacity(heap_classes.len());
        let jobs: Vec<HeapScanJob> = heap_classes
            .iter()
            .map(|&ci| {
                let sc = &db.classes[ci];
                let mine: Vec<usize> = trees
                    .keys()
                    .copied()
                    .filter(|s| specs[*s].owners.iter().any(|(j, _)| *j == ci))
                    .collect();
                let indexes = mine
                    .iter()
                    .map(|s| IndexKeys {
                        constraint: &specs[*s].owners.iter().find(|(j, _)| *j == ci).unwrap().1,
                        kd: &trees[s].kd,
                    })
                    .collect();
                feeds.push(mine);
                HeapScanJob {
                    target: HeapTarget {
                        name: db.class_name(sc.oid),
                        class_oid: sc.oid,
                        class: Some(&sc.class),
                        hfid: sc.class.heap,
                    },
                    indexes,
                }
            })
            .collect();
        let passes =
            scan_heaps(db, &jobs, lim.workers, lim.memory, lim.spill).map_err(build_err)?;
        for (((job, &ci), mine), p) in jobs.iter().zip(&heap_classes).zip(&feeds).zip(passes) {
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
            for ((s, run), n) in mine.iter().zip(p.runs.runs).zip(p.runs.nulls) {
                runs.entry(*s).or_default().append(run);
                *nulls.entry(*s).or_default() += n;
            }
        }
    }
    phases.push(("heap pass", t_heaps.elapsed()));
    let t_sort = Instant::now();
    // a tree's own defects (from open) are reported once, whatever its shards find
    for t in trees.values_mut() {
        r.defects.extend(std::mem::take(&mut t.defects));
    }
    let mut index_runs: Vec<(usize, IndexRuns, u64)> = trees
        .keys()
        .map(|s| {
            (
                *s,
                runs.remove(s).unwrap_or_default(),
                nulls.get(s).copied().unwrap_or(0),
            )
        })
        .collect();
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
    let plans = index_runs
        .iter()
        .map(|(s, run, _)| {
            let t = &trees[s];
            plan_shards(t, run, shard_count(t.leaves.len(), lim.workers.limit()))
        })
        .collect::<std::io::Result<Vec<_>>>()?;
    phases.push(("run sort", t_sort.elapsed()));

    let t_merge = Instant::now();
    // every shard of every plain index merged with its heap side, and every foreign key against its primary key, in
    // one pool on --workers threads, the most leaf pages first (workspace#503, #509)
    let fks = foreign_key_jobs(db, &constraints);
    r.foreign_keys = fks.len();
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
    phases.push(("index merge and foreign keys", t_merge.elapsed()));

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
