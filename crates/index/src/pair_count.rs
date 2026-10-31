//! Counts what an index rebuild's heap scan would turn into spill records: per index the bytes its (key, OID) pairs
//! take (`temp_disk::spill_record_bytes`), from one read of the owner classes' heaps in physical order (extents,
//! workspace#596), keeping no keys.
//!
//! Input: the volumes (the target after page treatment, or a source read only; an 11.4 source passes a
//! re-encoder per re-encoded class so its records are counted as the target will hold them), the index
//! definitions and the worker count. Output: per definition, per owner class, the pairs' [`PairStats`] — the input of
//! `temp_disk::index_batches`, of the plan's temp disk estimate and of the target space bound.
//! Rows are read with `heap_keys::page_keys`, the extractor the build uses, so every key counted is a key the build
//! emits (deleted rows and all-NULL keys are left out the same way). Records not yet on a page (the catalog rows an
//! 11.4 source's run generates) go through the same extractor (`record_pair_bytes`).
//! Source: workspace#526.

use std::collections::{BTreeMap, HashMap, HashSet};

use fastmig_ondisk::btree::{max_keylen_inpage, ovf_key};
use fastmig_ondisk::file_table::FileTable;
use fastmig_ondisk::heap_rewrite::{Growth, ReEncode};
use fastmig_ondisk::page_extents::extents;
use fastmig_ondisk::volume_set::VolumeSet;
use fastmig_ondisk::{Oid, db_pagesize};

use crate::build_error::BuildError;
use crate::heap_keys::{ClassKeys, page_bytes_keys_sized, record_keys};
use crate::heap_scan::RowKey;
use crate::index_def::IndexDef;
use crate::job_pool::run_jobs;
use crate::temp_disk::spill_record_bytes;

/// What the pairs of one index and owner class come to: the input of the temp disk figures (`bytes`) and of the
/// target space bound of the new B-trees (workspace#526).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PairStats {
    /// Spill record bytes (`temp_disk::spill_record_bytes` per pair).
    pub bytes: u64,
    pub pairs: u64,
    /// The longest key image.
    pub max_key: u64,
    /// Overflow key file pages of the pairs whose key goes there (`ovf_key::pages_for` per pair: a key held by
    /// several rows is counted once per row, an upper bound).
    pub ovf_key_pages: u64,
}

impl PairStats {
    fn add_key(&mut self, len: usize, db_pagesize: usize) {
        self.bytes += spill_record_bytes(len as u64);
        self.pairs += 1;
        self.max_key = self.max_key.max(len as u64);
        if len >= max_keylen_inpage(db_pagesize) {
            self.ovf_key_pages += ovf_key::pages_for(len, db_pagesize) as u64;
        }
    }

    pub fn merge(&mut self, o: &PairStats) {
        self.bytes += o.bytes;
        self.pairs += o.pairs;
        self.max_key = self.max_key.max(o.max_key);
        self.ovf_key_pages += o.ovf_key_pages;
    }
}

/// The pairs of one definition, by owner class OID.
pub type PairBytes = BTreeMap<Oid, PairStats>;

/// One owner class of `defs`: its key extractor and the definitions its indexes feed, in emit order.
struct OwnerClass<'c> {
    keys: ClassKeys<'c>,
    feeds: Vec<usize>,
}

fn owner_classes<'c>(defs: &[&'c IndexDef<'c>]) -> Result<Vec<OwnerClass<'c>>, BuildError> {
    // per owner class: (definition, owner position) of its first index and every definition it feeds
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
    by_class
        .into_iter()
        .map(|(class_oid, (d0, o0, feeds))| {
            let owner = &defs[d0].owners[o0];
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
            let keys = ClassKeys::new(
                defs[d0].shape.target,
                class_oid,
                owner.class,
                owner.hfid,
                indexes,
            )
            .map_err(|e| BuildError::Record {
                oid: class_oid,
                what: format!("class layout: {e}"),
            })?;
            Ok(OwnerClass { keys, feeds })
        })
        .collect()
}

/// User pages of each owner class's heap: what an index build's heap scan reads (its progress total, workspace#560).
pub fn owner_heap_pages<'c>(
    vols: &VolumeSet,
    defs: &[&'c IndexDef<'c>],
) -> Result<Vec<(Oid, u64)>, BuildError> {
    owner_classes(defs)?
        .iter()
        .map(|c| {
            let pages = FileTable::read(vols, c.keys.hfid.vfid)?.user_pages().len() as u64;
            Ok((c.keys.class_oid, pages))
        })
        .collect()
}

/// Adds one class's per-index bytes to the definitions it feeds.
fn add(out: &mut [PairBytes], c: &OwnerClass, stats: Vec<PairStats>) {
    for (&d, s) in c.feeds.iter().zip(stats) {
        out[d].entry(c.keys.class_oid).or_default().merge(&s);
    }
}

pub fn pair_bytes<'c>(
    vols: &VolumeSet,
    defs: &[&'c IndexDef<'c>],
    workers: usize,
    re_encode: &dyn Fn(Oid) -> Option<&'c dyn ReEncode>,
    skip: &HashSet<Oid>,
) -> Result<Vec<PairBytes>, BuildError> {
    Ok(pair_bytes_and_growth(vols, defs, workers, re_encode, skip)?.0)
}

/// [`pair_bytes`], and per owner class with a re-encoder the [`Growth`] of its re-encoded records: the pages its
/// page treatment may append, from the same read (workspace#526: `page_treatment::appended_pages_bound` skips these
/// classes).
pub fn pair_bytes_and_growth<'c>(
    vols: &VolumeSet,
    defs: &[&'c IndexDef<'c>],
    workers: usize,
    re_encode: &dyn Fn(Oid) -> Option<&'c dyn ReEncode>,
    skip: &HashSet<Oid>,
) -> Result<(Vec<PairBytes>, HashMap<Oid, Growth>), BuildError> {
    let mut classes = owner_classes(defs)?;
    let mut pages = Vec::new();
    for (i, c) in classes.iter_mut().enumerate() {
        if let Some(r) = re_encode(c.keys.class_oid) {
            c.keys.re_encode = Some(r);
        }
        pages.extend(
            FileTable::read(vols, c.keys.hfid.vfid)?
                .user_pages()
                .into_iter()
                .map(|v| (v, i)),
        );
    }
    let io_pagesize = vols.io_pagesize();
    // every owner heap in physical order, not class by class: the heaps interleave in a volume (workspace#596)
    let units = extents(pages, io_pagesize);
    let db = db_pagesize(io_pagesize);
    let usable = Growth::usable(io_pagesize);
    let counted = run_jobs(workers, units, |e| {
        e.read(vols, |buf| {
            let buf = buf?;
            // per class met in this extent: (class, per-index stats, growth); an extent holds few classes
            let mut tally: Vec<(usize, Vec<PairStats>, Growth)> = Vec::new();
            for (vpid, page, &c) in e.listed(buf, io_pagesize) {
                let t = match tally.iter().position(|(k, ..)| *k == c) {
                    Some(t) => t,
                    None => {
                        let n = classes[c].keys.indexes_len();
                        tally.push((c, vec![PairStats::default(); n], Growth::default()));
                        tally.len() - 1
                    }
                };
                let (_, stats, grown) = &mut tally[t];
                page_bytes_keys_sized(
                    vols,
                    &classes[c].keys,
                    page,
                    vpid,
                    &mut |ix, oid, key| {
                        if let RowKey::Key(k) = key
                            && !skip.contains(&oid)
                        {
                            stats[ix].add_key(k.len(), db);
                        }
                    },
                    &mut |before, after| grown.add(before, after, usable),
                )?;
            }
            Ok::<_, BuildError>(tally)
        })
    });
    let mut out: Vec<PairBytes> = defs.iter().map(|_| PairBytes::new()).collect();
    let mut growth: HashMap<Oid, Growth> = HashMap::new();
    for r in counted {
        for (c, stats, grown) in r? {
            add(&mut out, &classes[c], stats);
            if classes[c].keys.re_encode.is_some() {
                growth
                    .entry(classes[c].keys.class_oid)
                    .or_default()
                    .merge(&grown);
            }
        }
    }
    Ok((out, growth))
}

/// The same count over records not yet on any page: `records(class)` gives each owner class's rows as (OID, develop
/// record). An 11.4 source's system catalog rows, which the run generates (workspace#526).
pub fn record_pair_bytes<'c, 'r>(
    defs: &[&'c IndexDef<'c>],
    io_pagesize: usize,
    records: &dyn Fn(Oid) -> &'r [(Oid, Vec<u8>)],
) -> Result<Vec<PairBytes>, BuildError> {
    let mut out: Vec<PairBytes> = defs.iter().map(|_| PairBytes::new()).collect();
    for c in owner_classes(defs)? {
        let mut stats = vec![PairStats::default(); c.keys.indexes_len()];
        for (oid, rec) in records(c.keys.class_oid) {
            record_keys(&c.keys, *oid, rec, &mut |ix, _, key| {
                if let RowKey::Key(k) = key {
                    stats[ix].add_key(k.len(), db_pagesize(io_pagesize));
                }
            })?;
        }
        add(&mut out, &c, stats);
    }
    Ok(out)
}
