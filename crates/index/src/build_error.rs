//! Why an index build stops.
//!
//! Source: R6 § 4.3 R-LEAF-7 (unique violation, engine `btree_load.c:4393-4407`), R-SORT-2 (the same OID twice
//! under one key is a defect), and I/O of the page sinks. A NOT NULL violation (R-ROW-4) is reported by the key
//! extractor during the heap scan.

use fastmig_ondisk::Oid;
use std::fmt;

#[derive(Debug)]
pub enum BuildError {
    Io(std::io::Error),
    /// A unique index key carried by more than one row (a defect of the source database).
    UniqueViolation {
        key: Vec<u8>,
        oids: Vec<Oid>,
    },
    /// The same row twice under one key.
    DuplicateEntry {
        key: Vec<u8>,
        oid: Oid,
    },
    /// A NULL in a NOT NULL key column (R-ROW-4, engine `btree_load.c:6318-6328`).
    NotNullViolation {
        oid: Oid,
    },
}

impl From<std::io::Error> for BuildError {
    fn from(e: std::io::Error) -> Self {
        BuildError::Io(e)
    }
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BuildError::Io(e) => write!(f, "page I/O: {e}"),
            BuildError::UniqueViolation { key, oids } => {
                write!(
                    f,
                    "unique key {:02x?} carried by {} rows: {:?}",
                    key,
                    oids.len(),
                    oids
                )
            }
            BuildError::DuplicateEntry { key, oid } => {
                write!(f, "row {oid:?} twice under key {key:02x?}")
            }
            BuildError::NotNullViolation { oid } => {
                write!(f, "row {oid:?} has a NULL in a NOT NULL key")
            }
        }
    }
}

impl std::error::Error for BuildError {}
