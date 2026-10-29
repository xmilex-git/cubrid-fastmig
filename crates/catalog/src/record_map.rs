//! A [`ReEncode`] whose records are already made: the regenerated class objects of the root heap, the develop rows of
//! a same-slot system table.
//!
//! Input: OID → develop record bytes, whether the heap's class is MVCC, and what a live record the map lacks means.
//! Output: the record for an OID.
//! Source: workspace#499 (root heap and same-slot rows go through `rewrite_heap` with an OID → bytes re-encoder;
//! committed deletes are removed before the re-encoder is asked); the root class's own record is the same 40 bytes in
//! 11.4 and develop (T1 fm114_clean and fmdev_clean, 0|193|1, workspace#504), so the root heap keeps what it lacks.

use std::collections::HashMap;

use fastmig_ondisk::Oid;
use fastmig_ondisk::heap_rewrite::ReEncode;
use fastmig_ondisk::value::{DecodeError, DecodeResult};

/// What a live record without an entry in the map becomes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unmapped {
    /// Kept byte for byte (the root class record; class objects deleted afterwards).
    Keep,
    /// An error: every live row of the table was read and regenerated.
    Refuse,
}

pub struct RecordMap {
    pub records: HashMap<Oid, Vec<u8>>,
    pub mvcc: bool,
    pub unmapped: Unmapped,
}

impl ReEncode for RecordMap {
    fn re_encode(&self, oid: Oid, rec: &[u8]) -> DecodeResult<Vec<u8>> {
        match (self.records.get(&oid), self.unmapped) {
            (Some(r), _) => Ok(r.clone()),
            (None, Unmapped::Keep) => Ok(rec.to_vec()),
            (None, Unmapped::Refuse) => Err(DecodeError {
                at: 0,
                what: format!("live record {oid:?} has no regenerated row"),
            }),
        }
    }

    fn mvcc(&self) -> bool {
        self.mvcc
    }
}
