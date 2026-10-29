//! Check 2 (foreign keys): every key of a foreign key index with no NULL column is a key of the referenced primary
//! key — the union of the partitions' primary key indexes for a partitioned class.
//!
//! Input: the FK index tree and the referenced PK trees. Output: `fk-orphan-key` defects. FK keys are decoded with
//! the FK's key domain and re-encoded with the PK's, so both sides compare in the PK's order — or, when the two
//! domains are equal, compared as they are (NULL columns found from the image header); each side is a sorted stream
//! of distinct keys, merged once (O(n)).
//! The builder does not check this (it reports only unique and NOT NULL violations); the verifier does
//! (G3 workspace#483 Q4, after #478 Q8). An 11.3/11.4 DEDUPLICATE FK index drops its hidden key column first.
//! Source: G3 workspace#483 Q4; engine FK semantics (MATCH SIMPLE: a key with a NULL column references nothing).

use std::cmp::Ordering;

use fastmig_ondisk::VersionLine;
use fastmig_ondisk::value::{KeyDomain, Value};

use crate::btree_walk::{IndexTree, LeafPairs};
use crate::defect::{Check, Defect};

/// The distinct keys of one tree, ascending; the current one in a buffer reused from key to key.
struct DistinctKeys<'t, 'a> {
    pairs: LeafPairs<'t, 'a>,
    key: Vec<u8>,
    has: bool,
}

impl<'t, 'a> DistinctKeys<'t, 'a> {
    fn new(t: &'t IndexTree<'a>) -> Self {
        let mut s = DistinctKeys {
            pairs: LeafPairs::new(t),
            key: Vec::new(),
            has: false,
        };
        s.advance();
        s
    }

    /// Moves to the next distinct key; `has` false after the last.
    fn advance(&mut self) {
        while let Some((k, _)) = self.pairs.next_pair() {
            if !self.has || self.key != k {
                self.key.clear();
                self.key.extend_from_slice(k);
                self.has = true;
                return;
            }
        }
        self.has = false;
    }
}

pub fn check_fk(fk: &IndexTree, pks: &[IndexTree]) -> Vec<Defect> {
    let mut out = Vec::new();
    let Some(pk_kd) = pks.first().map(|t| &t.kd) else {
        return out;
    };
    let fk_kd: &KeyDomain = &fk.kd;
    // the same key domain on both sides: an FK image is its PK image, no decode and encode per key (workspace#509);
    // check 2 has compared the FK index keys with the heap already
    let same_domain = fk_kd == pk_kd;
    // an 11.3/11.4 DEDUPLICATE FK key carries a hidden SHORT column the PK has not (R16 workspace#532, workspace#537)
    let hidden = (fk.db.line == VersionLine::V11_4)
        .then(|| fk.root.dedup_key_idx_114())
        .flatten()
        .and_then(|i| usize::try_from(i).ok());
    let (class, index) = (fk.class.clone(), fk.name.clone());
    let mut fks = DistinctKeys::new(fk);
    let mut pk_streams: Vec<DistinctKeys> = pks.iter().map(DistinctKeys::new).collect();
    while fks.has {
        let k = &fks.key;
        let encoded;
        let want: &[u8] = if same_domain {
            match fk_kd.columns_of(k).find(|c| !matches!(c, Ok(Some(_)))) {
                None => k,
                Some(Ok(None)) => {
                    fks.advance();
                    continue;
                }
                Some(Err(e)) => {
                    out.push(decode_defect(&class, &index, e));
                    fks.advance();
                    continue;
                }
                Some(Ok(Some(_))) => unreachable!("filtered out by find"),
            }
        } else {
            match fk_kd.decode_key(k).map(|mut v| {
                if let Some(i) = hidden.filter(|&i| i < v.len()) {
                    v.remove(i);
                }
                v
            }) {
                Ok(v) if v.len() != pk_kd.columns.len() => {
                    out.push(decode_defect(
                        &class,
                        &index,
                        format!(
                            "{} key columns, the primary key has {}",
                            v.len(),
                            pk_kd.columns.len()
                        ),
                    ));
                    fks.advance();
                    continue;
                }
                Ok(v) if v.iter().any(Value::is_null) => {
                    fks.advance();
                    continue;
                }
                Ok(v) => {
                    encoded = pk_kd.encode_key(&v);
                    &encoded
                }
                Err(e) => {
                    out.push(decode_defect(&class, &index, e));
                    fks.advance();
                    continue;
                }
            }
        };
        // advance every PK stream past keys below `want`; found when one stream holds it
        let mut found = false;
        for s in pk_streams.iter_mut() {
            while s.has {
                match pk_kd.compare(&s.key, want) {
                    Ordering::Less => s.advance(),
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
                    .object(format!("{:?}", fk_kd.decode_key(k).unwrap_or_default()))
                    .expected("a key of the referenced primary key"),
            );
        }
        fks.advance();
    }
    out
}

fn decode_defect(class: &str, index: &str, e: impl std::fmt::Display) -> Defect {
    Defect::new(Check::IndexHeap, "fk-key-decode")
        .class(class.to_string())
        .index(index.to_string())
        .actual(e.to_string())
}
