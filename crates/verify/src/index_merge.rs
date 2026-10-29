//! Check 2 (pairs): merges an index's heap-side (key, OID) runs with its leaf pairs in one pass, both in key-then-OID
//! order, and reports the pairs only one side holds.
//!
//! Input: the heap pass's runs for the index (in-memory runs are sorted here, spilled ones already are), the index's
//! [`LeafPairs`]. Output: check 2 defects — `missing-in-index` (a live row without its entry), `missing-in-heap` (an
//! entry without its live row; the same OID under a second key lands here), `key-value-differs` (same position, the
//! decoded key values differ: e.g. two case variants under a case-insensitive collation).
//! Keys are equal when their decoded value tuples are (G3 Q1), not when their images are; the order is the key
//! domain's (`KeyDomain::compare`) then the OID. A deferred index (function or filter) has no heap side (G5): [`check_oids`]
//! checks its entries' OIDs name live rows of the indexed classes.
//! Source: G3 workspace#483 Q1, G5 workspace#486; k-way merge of the builder (`fastmig_index::kway_merge`).

use std::cmp::Ordering;
use std::io;

use fastmig_index::IndexRuns;
use fastmig_index::kway_merge::{Cursor, KeyStream, KwayMerge};
use fastmig_index::spill_run::{SpillCursor, SpillPos};
use fastmig_ondisk::Oid;
use fastmig_ondisk::value::KeyDomain;

use crate::btree_walk::LeafPairs;
use crate::defect::{Check, Defect, OidText};
use crate::live_oids::LiveOids;

/// Read buffer per spill file.
const MERGE_BUFFER: usize = 64 << 10;

fn key_text(kd: &KeyDomain, k: &[u8]) -> String {
    match kd.decode_key(k) {
        Ok(v) => format!("{v:?}"),
        Err(_) => format!("{k:02x?}"),
    }
}

pub fn merge_pairs(runs: &mut IndexRuns, leaves: &mut LeafPairs) -> io::Result<Vec<Defect>> {
    let kd = leaves.tree().kd.clone();
    let index = leaves.tree().name.clone();
    let class = leaves.tree().class.clone();
    for r in &mut runs.mem {
        r.sort(&kd);
    }
    let mut cursors = Vec::new();
    for run in &runs.mem {
        cursors.push(Cursor::Mem {
            run,
            at: 0,
            end: run.len(),
        });
    }
    for run in &runs.spilled {
        cursors.push(Cursor::File(SpillCursor::open(
            run,
            SpillPos {
                index: 0,
                offset: 0,
            },
            run.len(),
            MERGE_BUFFER,
        )?));
    }
    let mut heap = KwayMerge::new(cursors, &kd);
    let mut out = Vec::new();
    let d = |code, oid: Oid, key: &[u8]| {
        Defect::new(Check::IndexHeap, code)
            .class(class.clone())
            .index(index.clone())
            .object(OidText(oid))
            .at(format!("key {}", key_text(&kd, key)))
    };
    let mut h = heap.next_pair()?.map(|(k, o)| (k.to_vec(), o));
    let mut l = leaves.next_pair().map(|(k, o)| (k.to_vec(), o));
    loop {
        let ord = match (&h, &l) {
            (None, None) => break,
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (Some((hk, ho)), Some((lk, lo))) => kd.compare(hk, lk).then(ho.cmp(lo)),
        };
        match ord {
            Ordering::Equal => {
                let ((hk, ho), (lk, _)) = (h.as_ref().unwrap(), l.as_ref().unwrap());
                if hk != lk && kd.decode_key(hk).ok() != kd.decode_key(lk).ok() {
                    out.push(
                        d("key-value-differs", *ho, lk)
                            .expected(key_text(&kd, hk))
                            .actual(key_text(&kd, lk)),
                    );
                }
                h = heap.next_pair()?.map(|(k, o)| (k.to_vec(), o));
                l = leaves.next_pair().map(|(k, o)| (k.to_vec(), o));
            }
            Ordering::Less => {
                let (hk, ho) = h.as_ref().unwrap();
                out.push(d("missing-in-index", *ho, hk));
                h = heap.next_pair()?.map(|(k, o)| (k.to_vec(), o));
            }
            Ordering::Greater => {
                let (lk, lo) = l.as_ref().unwrap();
                out.push(d("missing-in-heap", *lo, lk));
                l = leaves.next_pair().map(|(k, o)| (k.to_vec(), o));
            }
        }
    }
    Ok(out)
}

/// A deferred index (function or filter): every entry names a live row of a class `owns` accepts.
pub fn check_oids(
    leaves: &mut LeafPairs,
    live: &LiveOids,
    owns: &dyn Fn(Oid) -> bool,
) -> Vec<Defect> {
    let kd = leaves.tree().kd.clone();
    let index = leaves.tree().name.clone();
    let class = leaves.tree().class.clone();
    let mut out = Vec::new();
    while let Some((k, oid)) = leaves.next_pair() {
        if !live.class_of(oid).is_some_and(owns) {
            out.push(
                Defect::new(Check::IndexHeap, "missing-in-heap")
                    .class(class.clone())
                    .index(index.clone())
                    .object(OidText(oid))
                    .at(format!("key {}", key_text(&kd, k))),
            );
        }
    }
    out
}
