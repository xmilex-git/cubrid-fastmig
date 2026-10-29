//! Check 2 (pairs): merges an index's heap-side (key, OID) runs with its leaf pairs in one pass, both in key-then-OID
//! order, and reports the pairs only one side holds.
//!
//! Input: the heap pass's sorted runs for the index (whole, or one shard of them: `merge_shards`), the index's
//! [`LeafPairs`] over the same leaves. Output: check 2 defects — `missing-in-index` (a live row without its entry),
//! `missing-in-heap` (an entry without its live row; the same OID under a second key lands here), `key-value-differs`
//! (same position, the decoded key values differ: e.g. two case variants under a case-insensitive collation).
//! Keys are equal when their decoded value tuples are (G3 Q1), not when their images are; the order is the key
//! domain's (`KeyDomain::compare`) then the OID; byte-equal images are equal without a compare. A deferred index
//! (function or filter) has no heap side (G5): [`check_oids`] checks its entries' OIDs name live rows of the indexed
//! classes.
//! Source: G3 workspace#483 Q1, G5 workspace#486; k-way merge of the builder (`fastmig_index::kway_merge`).

use std::cmp::Ordering;
use std::io;

use fastmig_index::IndexRuns;
use fastmig_index::intermediate_merge::MERGE_BUFFER;
use fastmig_index::kway_merge::{Cursor, KeyStream, KwayMerge};
use fastmig_index::spill_run::{SpillCursor, SpillPos};
use fastmig_index::splitter::ShardBounds;
use fastmig_ondisk::Oid;
use fastmig_ondisk::value::KeyDomain;

use crate::btree_walk::LeafPairs;
use crate::defect::{Check, Defect, OidText};
use crate::live_row_map::LiveRowMap;

fn key_text(kd: &KeyDomain, k: &[u8]) -> String {
    match kd.decode_key(k) {
        Ok(v) => format!("{v:?}"),
        Err(_) => format!("{k:02x?}"),
    }
}

/// Merges `runs` (sorted; shard `s` of `bounds` when given) with `leaves`.
pub fn merge_pairs(
    runs: &IndexRuns,
    shard: Option<(&ShardBounds, usize)>,
    leaves: &mut LeafPairs,
) -> io::Result<Vec<Defect>> {
    let kd = leaves.tree().kd.clone();
    let index = leaves.tree().name.clone();
    let class = leaves.tree().class.clone();
    let mut cursors = Vec::with_capacity(runs.mem.len() + runs.spilled.len());
    for (r, run) in runs.mem.iter().enumerate() {
        let (at, end) = match shard {
            Some((b, s)) => (b.mem[r][s], b.mem[r][s + 1]),
            None => (0, run.len()),
        };
        cursors.push(Cursor::Mem { run, at, end });
    }
    for (r, run) in runs.spilled.iter().enumerate() {
        let (from, end) = match shard {
            Some((b, s)) => (b.spilled[r][s], b.spilled[r][s + 1].index),
            None => (
                SpillPos {
                    index: 0,
                    offset: 0,
                },
                run.len(),
            ),
        };
        cursors.push(Cursor::File(SpillCursor::open(
            run,
            from,
            end,
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
    // each side's current key lives in its own buffer, refilled per pair (no allocation per row)
    let (mut hk, mut lk) = (Vec::new(), Vec::new());
    let mut h = take_pair(heap.next_pair()?, &mut hk);
    let mut l = take_pair(leaves.next_pair(), &mut lk);
    loop {
        let ord = match (h, l) {
            (None, None) => break,
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            // byte-equal images are equal keys; only differing images need the key domain's compare
            (Some(ho), Some(lo)) if hk == lk => ho.cmp(&lo),
            (Some(ho), Some(lo)) => kd.compare(&hk, &lk).then(ho.cmp(&lo)),
        };
        match ord {
            Ordering::Equal => {
                if hk != lk && kd.decode_key(&hk).ok() != kd.decode_key(&lk).ok() {
                    out.push(
                        d("key-value-differs", h.unwrap(), &lk)
                            .expected(key_text(&kd, &hk))
                            .actual(key_text(&kd, &lk)),
                    );
                }
                h = take_pair(heap.next_pair()?, &mut hk);
                l = take_pair(leaves.next_pair(), &mut lk);
            }
            Ordering::Less => {
                out.push(d("missing-in-index", h.unwrap(), &hk));
                h = take_pair(heap.next_pair()?, &mut hk);
            }
            Ordering::Greater => {
                out.push(d("missing-in-heap", l.unwrap(), &lk));
                l = take_pair(leaves.next_pair(), &mut lk);
            }
        }
    }
    Ok(out)
}

/// Copies a pair's key into `buf` and returns its OID.
fn take_pair(pair: Option<(&[u8], Oid)>, buf: &mut Vec<u8>) -> Option<Oid> {
    pair.map(|(k, o)| {
        buf.clear();
        buf.extend_from_slice(k);
        o
    })
}

/// A deferred index (function or filter): every entry names a live row of a class `owns` accepts.
pub fn check_oids(
    leaves: &mut LeafPairs,
    live: &LiveRowMap,
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
