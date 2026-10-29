//! Counts what an index rebuild's heap scan would turn into spill records: per index the bytes its (key, OID) pairs
//! take (`temp_disk::spill_record_bytes`), from one read of the owner classes' heaps, keeping no keys.
//!
//! Input: the volumes (the target after page treatment, or a source read only; an 11.4 source passes a
//! re-encoder per re-encoded class so its records are counted as the target will hold them), the index
//! definitions and the worker count. Output: per definition, per owner class, the pair bytes — the input of
//! `temp_disk::index_batches` and of the plan's temp disk estimate.
//! Rows are read with `heap_keys::page_keys`, the extractor the build uses, so every key counted is a key the build
//! emits (deleted rows and all-NULL keys are left out the same way). Records not yet on a page (the catalog rows an
//! 11.4 source's run generates) go through the same extractor (`record_pair_bytes`).
//! Source: workspace#526.

use std::collections::BTreeMap;

use fastmig_ondisk::Oid;
use fastmig_ondisk::file_table::FileTable;
use fastmig_ondisk::heap_rewrite::ReEncode;
use fastmig_ondisk::volume_set::VolumeSet;

use crate::build_error::BuildError;
use crate::heap_keys::{ClassKeys, page_keys, record_keys};
use crate::heap_scan::RowKey;
use crate::index_def::IndexDef;
use crate::job_pool::run_jobs;
use crate::temp_disk::spill_record_bytes;

/// Heap pages per counting job: enough to keep a job's setup negligible, small enough to spread a large heap.
const UNIT_PAGES: usize = 256;

/// Pair bytes of one definition, by owner class OID.
pub type PairBytes = BTreeMap<Oid, u64>;

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
            let keys =
                ClassKeys::new(class_oid, owner.class, owner.hfid, indexes).map_err(|e| {
                    BuildError::Record {
                        oid: class_oid,
                        what: format!("class layout: {e}"),
                    }
                })?;
            Ok(OwnerClass { keys, feeds })
        })
        .collect()
}

/// Adds one class's per-index bytes to the definitions it feeds.
fn add(out: &mut [PairBytes], c: &OwnerClass, bytes: Vec<u64>) {
    for (&d, b) in c.feeds.iter().zip(bytes) {
        *out[d].entry(c.keys.class_oid).or_default() += b;
    }
}

pub fn pair_bytes<'c>(
    vols: &VolumeSet,
    defs: &[&'c IndexDef<'c>],
    workers: usize,
    re_encode: &dyn Fn(Oid) -> Option<&'c dyn ReEncode>,
) -> Result<Vec<PairBytes>, BuildError> {
    let mut classes = owner_classes(defs)?;
    let mut units = Vec::new();
    for (i, c) in classes.iter_mut().enumerate() {
        if let Some(r) = re_encode(c.keys.class_oid) {
            c.keys.re_encode = Some(r);
        }
        let pages: Vec<_> = FileTable::read(vols, c.keys.hfid.vfid)?
            .user_pages_by_sector()
            .into_iter()
            .flat_map(|(_, p)| p)
            .collect();
        for chunk in pages.chunks(UNIT_PAGES) {
            units.push((i, chunk.to_vec()));
        }
    }
    units.sort_by_key(|(_, p)| std::cmp::Reverse(p.len()));
    let io_pagesize = vols.io_pagesize();
    let counted = run_jobs(workers, units, |(c, pages)| {
        let mut buf = vec![0u8; io_pagesize];
        let mut bytes = vec![0u64; classes[c].keys.indexes_len()];
        for vpid in pages {
            page_keys(vols, &classes[c].keys, &mut buf, vpid, &mut |ix, _, key| {
                if let RowKey::Key(k) = key {
                    bytes[ix] += spill_record_bytes(k.len() as u64);
                }
            })?;
        }
        Ok::<_, BuildError>((c, bytes))
    });
    let mut out: Vec<PairBytes> = defs.iter().map(|_| PairBytes::new()).collect();
    for r in counted {
        let (c, bytes) = r?;
        add(&mut out, &classes[c], bytes);
    }
    Ok(out)
}

/// The same count over records not yet on any page: `records(class)` gives each owner class's rows as (OID, develop
/// record). An 11.4 source's system catalog rows, which the run generates (workspace#526).
pub fn record_pair_bytes<'c, 'r>(
    defs: &[&'c IndexDef<'c>],
    records: &dyn Fn(Oid) -> &'r [(Oid, Vec<u8>)],
) -> Result<Vec<PairBytes>, BuildError> {
    let mut out: Vec<PairBytes> = defs.iter().map(|_| PairBytes::new()).collect();
    for c in owner_classes(defs)? {
        let mut bytes = vec![0u64; c.keys.indexes_len()];
        for (oid, rec) in records(c.keys.class_oid) {
            record_keys(&c.keys, *oid, rec, &mut |ix, _, key| {
                if let RowKey::Key(k) = key {
                    bytes[ix] += spill_record_bytes(k.len() as u64);
                }
            })?;
        }
        add(&mut out, &c, bytes);
    }
    Ok(out)
}
