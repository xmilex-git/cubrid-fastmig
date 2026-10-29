//! Counts what an index rebuild's heap scan would turn into spill records: per index the bytes its (key, OID) pairs
//! take (`temp_disk::spill_record_bytes`), from one read of the owner classes' heaps, keeping no keys.
//!
//! Input: the volumes (the target after page treatment, or a source read only; an 11.4 source passes a
//! re-encoder per re-encoded class so its records are counted as the target will hold them), the index
//! definitions and the worker count. Output: per definition, per owner class, the pair bytes — the input of
//! `temp_disk::index_batches` and of the plan's temp disk estimate.
//! Rows are read with `heap_keys::page_keys`, the extractor the build uses, so every key counted is a key the build
//! emits (deleted rows and all-NULL keys are left out the same way).
//! Source: workspace#526.

use std::collections::BTreeMap;

use fastmig_ondisk::Oid;
use fastmig_ondisk::file_table::FileTable;
use fastmig_ondisk::heap_rewrite::ReEncode;
use fastmig_ondisk::volume_set::VolumeSet;

use crate::build_error::BuildError;
use crate::heap_keys::{ClassKeys, page_keys};
use crate::heap_scan::RowKey;
use crate::index_def::IndexDef;
use crate::job_pool::run_jobs;
use crate::temp_disk::spill_record_bytes;

/// Heap pages per counting job: enough to keep a job's setup negligible, small enough to spread a large heap.
const UNIT_PAGES: usize = 256;

/// Pair bytes of one definition, by owner class OID.
pub type PairBytes = BTreeMap<Oid, u64>;

pub fn pair_bytes<'c>(
    vols: &VolumeSet,
    defs: &[&'c IndexDef<'c>],
    workers: usize,
    re_encode: &dyn Fn(Oid) -> Option<&'c dyn ReEncode>,
) -> Result<Vec<PairBytes>, BuildError> {
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
    let mut keys = Vec::with_capacity(by_class.len());
    let mut units = Vec::new();
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
        let ck = ClassKeys::new(class_oid, owner.class, owner.hfid, indexes).map_err(|e| {
            BuildError::Record {
                oid: class_oid,
                what: format!("class layout: {e}"),
            }
        })?;
        let ck = match re_encode(class_oid) {
            Some(r) => ck.with_re_encode(r),
            None => ck,
        };
        let pages: Vec<_> = FileTable::read(vols, owner.hfid.vfid)?
            .user_pages_by_sector()
            .into_iter()
            .flat_map(|(_, p)| p)
            .collect();
        for chunk in pages.chunks(UNIT_PAGES) {
            units.push((keys.len(), chunk.to_vec()));
        }
        keys.push(ck);
    }
    units.sort_by_key(|(_, p)| std::cmp::Reverse(p.len()));
    let io_pagesize = vols.io_pagesize();
    let counted = run_jobs(workers, units, |(c, pages)| {
        let mut buf = vec![0u8; io_pagesize];
        let mut bytes = vec![0u64; keys[c].indexes_len()];
        for vpid in pages {
            page_keys(vols, &keys[c], &mut buf, vpid, &mut |ix, _, key| {
                if let RowKey::Key(k) = key {
                    bytes[ix] += spill_record_bytes(k.len() as u64);
                }
            })?;
        }
        Ok::<_, BuildError>((c, bytes))
    });
    let classes: Vec<(&Oid, &Vec<usize>)> = by_class.iter().map(|(o, (_, _, f))| (o, f)).collect();
    let mut out: Vec<PairBytes> = defs.iter().map(|_| PairBytes::new()).collect();
    for r in counted {
        let (c, bytes) = r?;
        let (&class_oid, feeds) = classes[c];
        for (&d, b) in feeds.iter().zip(bytes) {
            *out[d].entry(class_oid).or_default() += b;
        }
    }
    Ok(out)
}
