//! Where the builder puts pages: one sink per file (the B-tree file, and its overflow key file if any key
//! overflows).
//!
//! Input: whole IO pages (`io_pagesize` bytes, FILEIO header included). Output: pages on disk.
//! Source: R6 § 2 steps 8–11 (the engine's page provider); new pages carry a NULL LSA (workspace#482 G2 item 10).
//! The target-volume implementation comes with file allocation (workspace#490); `ScratchFileSink` serves a
//! stand-alone file.

use fastmig_ondisk::{Vfid, Vpid};
use std::io;

pub trait PageSink: Sync {
    fn io_pagesize(&self) -> usize;

    /// The file the pages belong to (root header `ovfid` names the overflow key file by it).
    fn vfid(&self) -> Vfid;

    /// A new page of this file (a B-tree root is allocated with the file, before the build: `btree_file`).
    fn alloc_page(&self) -> io::Result<Vpid>;

    /// Writes a whole IO page at `vpid`, which `alloc_page` returned.
    fn write_page(&self, vpid: Vpid, page: &[u8]) -> io::Result<()>;
}
