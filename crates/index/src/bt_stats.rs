//! The B-tree statistics the catalog representation stores for the index (`BTREE_STATS`), counted while
//! building.
//!
//! Source: engine `BTREE_STATS` (statistics.h, develop 5b1867174); workspace#482 G2 item 7 — the index builder
//! fills leafs/pages/height/keys/pkeys. `leafs` counts leaf pages and overflow OID pages ("leaf pages including
//! overflow pages"); `pages` counts every page of the B-tree file; the overflow key file is a file of its own.

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BtStats {
    pub leafs: u64,
    pub pages: u64,
    pub height: u64,
    pub keys: u64,
    /// `pkeys[i]`: distinct prefixes of the first `i + 1` key columns; the last equals `keys`.
    pub pkeys: Vec<u64>,
}
