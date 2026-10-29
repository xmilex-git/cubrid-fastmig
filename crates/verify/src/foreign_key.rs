//! Check 2 (foreign keys): every key of a foreign key index with no NULL column is a key of the referenced primary
//! key — the union of the partitions' primary key indexes for a partitioned class.
//!
//! Input: the FK index tree and the referenced PK trees. Output: `fk-orphan-key` defects. FK keys are decoded with
//! the FK's key domain and re-encoded with the PK's, so both sides compare in the PK's order; each side is a sorted
//! stream of distinct keys, merged once (O(n)).
//! The builder does not check this (it reports only unique and NOT NULL violations); the verifier does
//! (G3 workspace#483 Q4, after #478 Q8).
//! Source: G3 workspace#483 Q4; engine FK semantics (MATCH SIMPLE: a key with a NULL column references nothing).

use std::cmp::Ordering;

use fastmig_ondisk::value::{KeyDomain, Value};

use crate::btree_walk::{IndexTree, LeafPairs};
use crate::defect::{Check, Defect};

/// The distinct keys of one tree, ascending.
struct DistinctKeys<'t, 'a> {
    pairs: LeafPairs<'t, 'a>,
    last: Option<Vec<u8>>,
}

impl DistinctKeys<'_, '_> {
    fn next_key(&mut self) -> Option<Vec<u8>> {
        while let Some((k, _)) = self.pairs.next_pair() {
            if self.last.as_deref() != Some(k) {
                self.last = Some(k.to_vec());
                return self.last.clone();
            }
        }
        None
    }
}

pub fn check_fk(fk: &mut IndexTree, pks: &mut [IndexTree]) -> Vec<Defect> {
    let mut out = Vec::new();
    let Some(pk_kd) = pks.first().map(|t| t.kd.clone()) else {
        return out;
    };
    let fk_kd: KeyDomain = fk.kd.clone();
    let (class, index) = (fk.class.clone(), fk.name.clone());
    let mut fks = DistinctKeys {
        pairs: LeafPairs::new(fk),
        last: None,
    };
    let mut pk_streams: Vec<DistinctKeys> = pks
        .iter_mut()
        .map(|t| DistinctKeys {
            pairs: LeafPairs::new(t),
            last: None,
        })
        .collect();
    let mut heads: Vec<Option<Vec<u8>>> = pk_streams.iter_mut().map(|s| s.next_key()).collect();
    while let Some(k) = fks.next_key() {
        let vals = match fk_kd.decode_key(&k) {
            Ok(v) => v,
            Err(e) => {
                out.push(
                    Defect::new(Check::IndexHeap, "fk-key-decode")
                        .class(class.clone())
                        .index(index.clone())
                        .actual(e),
                );
                continue;
            }
        };
        if vals.iter().any(Value::is_null) {
            continue;
        }
        let want = pk_kd.encode_key(&vals);
        // advance every PK stream past keys below `want`; found when one stream holds it
        let mut found = false;
        for (s, h) in pk_streams.iter_mut().zip(heads.iter_mut()) {
            while let Some(cur) = h {
                match pk_kd.compare(cur, &want) {
                    Ordering::Less => *h = s.next_key(),
                    Ordering::Equal => {
                        found = true;
                        break;
                    }
                    Ordering::Greater => break,
                }
            }
        }
        if !found {
            out.push(
                Defect::new(Check::IndexHeap, "fk-orphan-key")
                    .class(class.clone())
                    .index(index.clone())
                    .object(format!("{vals:?}"))
                    .expected("a key of the referenced primary key"),
            );
        }
    }
    out
}
