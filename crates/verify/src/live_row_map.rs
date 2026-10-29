//! The live rows of the database: every OID a heap pass found live, with the class whose heap holds it.
//!
//! Input: per heap, its class OID and live OIDs. Output: a sorted set answering "is this OID a live row, and of which
//! class" in O(log n) (index → heap OID survival of deferred indexes, OBJECT values, catalog references).
//! Source: G3 workspace#483 Q7 ("live OID 정렬 집합과 병합"), G5 workspace#486 (deferred indexes: OID survival
//! only).

use fastmig_ondisk::Oid;

#[derive(Default)]
pub struct LiveOids {
    // shortcut: 16 B per live row held in memory, not under --mem; spill through sorted runs when a database's row
    // count makes this the largest allocation of a verify run
    oids: Vec<(Oid, Oid)>,
}

impl LiveOids {
    pub fn add(&mut self, class: Oid, oids: &[Oid]) {
        self.oids.extend(oids.iter().map(|&o| (o, class)));
    }

    /// Sorts; call once after the last `add`. Returns the OIDs found live twice (in two heaps or twice in one).
    pub fn seal(&mut self) -> Vec<(Oid, Oid, Oid)> {
        self.oids.sort_unstable();
        let dups = self
            .oids
            .windows(2)
            .filter(|w| w[0].0 == w[1].0)
            .map(|w| (w[0].0, w[0].1, w[1].1))
            .collect();
        self.oids.dedup_by_key(|e| e.0);
        dups
    }

    pub fn class_of(&self, oid: Oid) -> Option<Oid> {
        self.oids
            .binary_search_by_key(&oid, |e| e.0)
            .ok()
            .map(|i| self.oids[i].1)
    }

    pub fn len(&self) -> usize {
        self.oids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.oids.is_empty()
    }
}
