//! Merges slices of sorted runs, in memory or spilled, into one stream in index order.
//!
//! Input: one range of each in-memory run and of each spill file. Output: (key, OID) pairs, key then OID ascending.
//! Source: engine `btsort_put_result_index_leaf` k-way merge (btree_sort.c, develop 5b1867174); R6 § 2 step 8.

use crate::key_order::KeyOrder;
use crate::sort_run::Run;
use crate::spill_run::SpillCursor;
use fastmig_ondisk::Oid;
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::io;

pub enum Cursor<'a> {
    Mem { run: &'a Run, at: usize, end: usize },
    File(SpillCursor),
}

impl Cursor<'_> {
    fn key(&self) -> &[u8] {
        match self {
            Cursor::Mem { run, at, .. } => run.key(*at),
            Cursor::File(c) => c.key(),
        }
    }

    fn oid(&self) -> Oid {
        match self {
            Cursor::Mem { run, at, .. } => run.oid(*at),
            Cursor::File(c) => c.oid(),
        }
    }

    fn valid(&self) -> bool {
        match self {
            Cursor::Mem { at, end, .. } => at < end,
            Cursor::File(c) => c.valid(),
        }
    }

    fn advance(&mut self) -> io::Result<()> {
        match self {
            Cursor::Mem { at, .. } => {
                *at += 1;
                Ok(())
            }
            Cursor::File(c) => c.advance(),
        }
    }
}

struct Head<'a> {
    cur: Cursor<'a>,
    order: &'a dyn KeyOrder,
}

impl Head<'_> {
    fn cmp_entry(&self, other: &Self) -> Ordering {
        self.order
            .compare(self.cur.key(), other.cur.key())
            .then(self.cur.oid().cmp(&other.cur.oid()))
    }
}

impl PartialEq for Head<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.cmp_entry(other) == Ordering::Equal
    }
}
impl Eq for Head<'_> {}
impl PartialOrd for Head<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Head<'_> {
    // reversed: BinaryHeap is a max-heap
    fn cmp(&self, other: &Self) -> Ordering {
        other.cmp_entry(self)
    }
}

/// A stream of (key, OID) pairs whose key borrows the stream until the next call.
pub trait KeyStream {
    fn next_pair(&mut self) -> io::Result<Option<(&[u8], Oid)>>;
}

pub struct KwayMerge<'a> {
    heap: BinaryHeap<Head<'a>>,
    key: Vec<u8>,
}

impl<'a> KwayMerge<'a> {
    pub fn new(cursors: Vec<Cursor<'a>>, order: &'a dyn KeyOrder) -> Self {
        let heap = cursors
            .into_iter()
            .filter(Cursor::valid)
            .map(|cur| Head { cur, order })
            .collect();
        Self {
            heap,
            key: Vec::new(),
        }
    }
}

impl KeyStream for KwayMerge<'_> {
    fn next_pair(&mut self) -> io::Result<Option<(&[u8], Oid)>> {
        let Some(mut top) = self.heap.peek_mut() else {
            return Ok(None);
        };
        self.key.clear();
        self.key.extend_from_slice(top.cur.key());
        let oid = top.cur.oid();
        top.cur.advance()?;
        if !top.cur.valid() {
            std::collections::binary_heap::PeekMut::pop(top);
        }
        Ok(Some((&self.key, oid)))
    }
}
