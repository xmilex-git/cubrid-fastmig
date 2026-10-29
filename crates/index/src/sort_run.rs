//! One worker's run: the (key, OID) pairs it extracted, sorted by key then OID.
//!
//! Input: pairs pushed by one producer (`RunWriter`) with their radix words. Output: the run in index order.
//! Source: R6 § 2 steps 4–6 and § 4.2 R-SORT-1/2/4; the sort is the MSD radix of PR CUBRID/cubrid#8209
//! (`btree_sort_radix.cpp`: a byte digit at a time over the key's radix words, a compare sort below 64
//! entries or where the words run out), tie-broken by OID as `compare_driver` (btree_load.c, develop 5b1867174).

use crate::key_order::KeyOrder;
use fastmig_ondisk::Oid;
use std::cmp::Ordering;

/// Below this many entries a compare sort beats another counting pass (#8209 measured 64 best).
const RADIX_MIN_ENTRIES: usize = 64;

#[derive(Clone, Copy)]
struct Entry {
    key_off: usize,
    key_len: u32,
    words_off: u32,
    words_len: u32,
    oid: Oid,
}

/// Pairs of one worker. Keys live in one byte arena; entries point into it.
#[derive(Default)]
pub struct Run {
    keys: Vec<u8>,
    entries: Vec<Entry>,
    words: Vec<u64>,
}

impl Run {
    /// Appends a pair with its radix words (`KeyOrder::radix_words` of `key`).
    pub fn push(&mut self, key: &[u8], oid: Oid, words: &[u64]) {
        self.entries.push(Entry {
            key_off: self.keys.len(),
            key_len: key.len() as u32,
            words_off: self.words.len() as u32,
            words_len: words.len() as u32,
            oid,
        });
        self.keys.extend_from_slice(key);
        self.words.extend_from_slice(words);
    }

    /// Memory the run holds, counting allocated capacity and the spare entry array its sort takes.
    pub fn bytes(&self) -> usize {
        Self::footprint(
            self.keys.capacity(),
            self.words.capacity(),
            self.entries.capacity(),
            self.entries.len(),
        )
    }

    /// `bytes()` once a pair with `key_len` key bytes and `nwords` words is pushed (vectors grow by doubling).
    pub fn bytes_after_push(&self, key_len: usize, nwords: usize) -> usize {
        let grow = |len: usize, cap: usize, add: usize| {
            if len + add > cap {
                (cap * 2).max(len + add)
            } else {
                cap
            }
        };
        Self::footprint(
            grow(self.keys.len(), self.keys.capacity(), key_len),
            grow(self.words.len(), self.words.capacity(), nwords),
            grow(self.entries.len(), self.entries.capacity(), 1),
            self.entries.len() + 1,
        )
    }

    fn footprint(keys: usize, words: usize, entries_cap: usize, entries: usize) -> usize {
        keys + words * 8 + (entries_cap + entries) * std::mem::size_of::<Entry>()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn key(&self, i: usize) -> &[u8] {
        let e = &self.entries[i];
        &self.keys[e.key_off..e.key_off + e.key_len as usize]
    }

    pub fn oid(&self, i: usize) -> Oid {
        self.entries[i].oid
    }

    /// Sorts by key (`order`), then OID ascending (R-SORT-2); drops the radix words afterwards.
    pub fn sort(&mut self, order: &dyn KeyOrder) {
        let mut entries = std::mem::take(&mut self.entries);
        let mut spare = entries.clone();
        self.msd(&mut entries, &mut spare, 0, order);
        self.entries = entries;
        self.words = Vec::new();
    }

    /// Empties the run, keeping its allocations for the next pairs.
    pub fn clear(&mut self) {
        self.keys.clear();
        self.entries.clear();
        self.words.clear();
    }

    fn full_cmp(&self, a: &Entry, b: &Entry, order: &dyn KeyOrder) -> Ordering {
        let ka = &self.keys[a.key_off..a.key_off + a.key_len as usize];
        let kb = &self.keys[b.key_off..b.key_off + b.key_len as usize];
        order.compare(ka, kb).then(a.oid.cmp(&b.oid))
    }

    /// Byte `depth` of the entry's radix words (big-end first), or `None` past its last word.
    fn digit(&self, e: &Entry, depth: usize) -> Option<u8> {
        let w = depth / 8;
        (w < e.words_len as usize)
            .then(|| (self.words[e.words_off as usize + w] >> (56 - 8 * (depth % 8))) as u8)
    }

    fn msd(&self, part: &mut [Entry], spare: &mut [Entry], depth: usize, order: &dyn KeyOrder) {
        if part.len() < RADIX_MIN_ENTRIES || part.iter().any(|e| self.digit(e, depth).is_none()) {
            // words exhausted for some entry: from here the words no longer decide, the comparator does
            part.sort_unstable_by(|a, b| self.full_cmp(a, b, order));
            return;
        }
        let mut count = [0usize; 256];
        for e in part.iter() {
            count[self.digit(e, depth).unwrap() as usize] += 1;
        }
        let mut start = [0usize; 256];
        let mut acc = 0;
        for d in 0..256 {
            start[d] = acc;
            acc += count[d];
        }
        let mut pos = start;
        for e in part.iter() {
            let d = self.digit(e, depth).unwrap() as usize;
            spare[pos[d]] = *e;
            pos[d] += 1;
        }
        part.copy_from_slice(&spare[..part.len()]);
        for d in 0..256 {
            if count[d] > 1 {
                let r = start[d]..start[d] + count[d];
                self.msd(&mut part[r.clone()], &mut spare[r], depth + 1, order);
            }
        }
    }
}
