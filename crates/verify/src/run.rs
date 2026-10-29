//! Runs every check on one database and returns its defects, what could not be checked, counts and phase times.
//!
//! Input: an open [`Database`], the classes whose heap header statistics the migration measured, and the shared
//! worker / memory / spill limits. Output: [`DbResult`].
//! Order: check 4 (sectors; yields the tracked files) → per group of classes that share an index: one heap pass per
//! class (checks 1 and the heap side of 2), then each of the group's indexes walked and merged with its heap side
//! (check 2) → the root heap → deferred indexes against the live OID set → foreign keys → catalog references
//! and OBJECT values (check 3) → LOB files (check 5).
//! Source: G3 workspace#483 Q9 (one scan per class, all its indexes at once; merges per index; workers and --mem of
//! migrate), G5 workspace#486 (deferred indexes: structure + OID survival, key values "not checked").

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use fastmig_index::{BuildError, IndexRuns, MemoryLimit, SpillDir, WorkerLimit};
use fastmig_ondisk::class_object::{Constraint, class_properties};
use fastmig_ondisk::{Btid, Oid};

use crate::btree_walk::{IndexTree, LeafPairs};
use crate::catalog_refs::{CatalogInput, check_catalog};
use crate::database::Database;
use crate::defect::{Check, Defect, OidText};
use crate::foreign_key::check_fk;
use crate::heap_chain::check_chain;
use crate::heap_pass::{HeapTarget, IndexKeys, scan_heap};
use crate::index_merge::{check_oids, merge_pairs};
use crate::live_oids::LiveOids;
use crate::lob_files::check_lobs;
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

    let mut live = LiveOids::default();
    let mut refs: Vec<ObjectRef> = Vec::new();
    let mut deferred: Vec<(usize, IndexTree)> = Vec::new();
    let t_heaps = Instant::now();
    for group in groups(db, &specs) {
        let mut runs: BTreeMap<usize, IndexRuns> = BTreeMap::new();
        let mut nulls: BTreeMap<usize, u64> = BTreeMap::new();
        let mut trees: BTreeMap<usize, IndexTree> = BTreeMap::new();
        for &s in &group.indexes {
            let spec = &specs[s];
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
        for &ci in &group.classes {
            let sc = &db.classes[ci];
            let mine: Vec<usize> = trees
                .keys()
                .copied()
                .filter(|s| specs[*s].owners.iter().any(|(j, _)| *j == ci))
                .collect();
            let keys: Vec<IndexKeys> = mine
                .iter()
                .map(|s| IndexKeys {
                    constraint: &specs[*s].owners.iter().find(|(j, _)| *j == ci).unwrap().1,
                    kd: &trees[s].kd,
                })
                .collect();
            let t = HeapTarget {
                name: db.class_name(sc.oid),
                class_oid: sc.oid,
                class: Some(&sc.class),
                hfid: sc.class.heap,
            };
            let p =
                scan_heap(db, &t, &keys, lim.workers, lim.memory, lim.spill).map_err(build_err)?;
            r.heaps += 1;
            r.defects
                .extend(check_chain(&t, &p, measured.contains(&sc.oid)));
            r.defects
                .extend(check_lobs(&p.lobs, db.lob_base.as_deref(), &t.name));
            r.lobs += p.lobs.len();
            live.add(sc.oid, &p.live);
            refs.extend(p.refs);
            r.defects.extend(p.defects);
            for ((s, run), n) in mine.iter().zip(p.runs.runs).zip(p.runs.nulls) {
                runs.entry(*s).or_default().append(run);
                *nulls.entry(*s).or_default() += n;
            }
        }
        // the group's indexes, merged in parallel (each holds its own runs and tree)
        let results: Vec<std::io::Result<(Vec<Defect>, u64)>> = std::thread::scope(|sc| {
            let handles: Vec<_> = trees
                .iter_mut()
                .map(|(s, t)| {
                    let mut run = runs.remove(s).unwrap_or_default();
                    let n = nulls.get(s).copied().unwrap_or(0);
                    sc.spawn(move || {
                        let _permit = lim.workers.acquire(1);
                        let mut lp = LeafPairs::new(t);
                        let mut d = merge_pairs(&mut run, &mut lp)?;
                        let pairs = lp.live_objects as u64;
                        d.extend(lp.finish(Some(n)));
                        Ok((d, pairs))
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().expect("index merge panicked"))
                .collect()
        });
        for res in results {
            let (d, pairs) = res?;
            r.defects.extend(d);
            r.index_pairs += pairs;
        }
    }
    phases.push(("heaps and indexes", t_heaps.elapsed()));

    phase(&mut phases, "root heap", || -> std::io::Result<()> {
        let t = HeapTarget {
            name: "<root>".into(),
            class_oid: db.boot.rootclass_oid,
            class: None,
            hfid: db.boot.rootclass_hfid,
        };
        let p = scan_heap(db, &t, &[], lim.workers, lim.memory, lim.spill).map_err(build_err)?;
        r.heaps += 1;
        r.defects.extend(check_chain(&t, &p, false));
        live.add(db.boot.rootclass_oid, &p.live);
        r.defects.extend(p.defects);
        Ok(())
    })?;
    for (oid, a, b) in live.seal() {
        r.defects.push(
            Defect::new(Check::HeapChain, "row-in-two-heaps")
                .object(OidText(oid))
                .actual(format!("{} and {}", db.class_name(a), db.class_name(b))),
        );
    }
    r.live_rows = live.len();

    phase(&mut phases, "deferred indexes", || {
        for (s, mut t) in deferred {
            let owners: BTreeSet<Oid> = specs[s]
                .owners
                .iter()
                .map(|(j, _)| db.classes[*j].oid)
                .collect();
            let mut lp = LeafPairs::new(&mut t);
            let mut d = check_oids(&mut lp, &live, &|c| owners.contains(&c));
            r.index_pairs += lp.live_objects as u64;
            d.extend(lp.finish(None));
            r.defects.extend(d);
        }
    });

    phase(&mut phases, "foreign keys", || {
        let (d, n) = foreign_keys(db, &constraints);
        r.foreign_keys = n;
        r.defects.extend(d)
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

/// Classes that share an index, with those indexes; a class with no shared index is a group of its own.
struct Group {
    classes: Vec<usize>,
    indexes: Vec<usize>,
}

fn groups(db: &Database, specs: &[IndexSpec]) -> Vec<Group> {
    let n = db.classes.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut [usize], mut x: usize) -> usize {
        while p[x] != x {
            p[x] = p[p[x]];
            x = p[x];
        }
        x
    }
    for s in specs {
        let a = find(&mut parent, s.owners[0].0);
        for (j, _) in &s.owners[1..] {
            let b = find(&mut parent, *j);
            parent[b] = a;
        }
    }
    let mut out: BTreeMap<usize, Group> = BTreeMap::new();
    for (i, c) in db.classes.iter().enumerate() {
        if !c.class.heap.is_null() {
            let root = find(&mut parent, i);
            out.entry(root)
                .or_insert_with(|| Group {
                    classes: Vec::new(),
                    indexes: Vec::new(),
                })
                .classes
                .push(i);
        }
    }
    for (s, spec) in specs.iter().enumerate() {
        let root = find(&mut parent, spec.owners[0].0);
        if let Some(g) = out.get_mut(&root) {
            g.indexes.push(s);
        }
    }
    out.into_values().collect()
}

/// Every foreign key against its referenced primary key (with the partitions' primary keys of a partitioned class).
fn foreign_keys(
    db: &Database,
    constraints: &[Result<Vec<Constraint>, String>],
) -> (Vec<Defect>, usize) {
    let mut out = Vec::new();
    let mut n = 0;
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
            let mut pks: Vec<IndexTree> = pk_classes
                .iter()
                .filter_map(|&ci| {
                    let cons = constraints[ci].as_ref().ok()?;
                    let pk = cons
                        .iter()
                        .find(|c| c.name == ref_name && c.btid.is_some())?;
                    open(ci, pk, pk.btid?)
                })
                .collect();
            let Some(mut fk_tree) = open(i, k, btid) else {
                continue; // check 2 reports the unreadable index
            };
            // the walks above already ran in check 2; only the key comparison is new here
            n += 1;
            out.extend(check_fk(&mut fk_tree, &mut pks));
        }
    }
    (out, n)
}
