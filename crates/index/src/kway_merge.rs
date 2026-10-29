//! Merges slices of sorted runs into one stream in index order.
//!
//! Input: one range of each sorted run. Output: (key, OID) pairs, key then OID ascending.
//! Source: engine `btsort_put_result_index_leaf` k-way merge (btree_sort.c, develop 5b1867174); R6 § 2 step 8.

use crate::key_order::KeyOrder;
use crate::sort_run::Run;
use fastmig_ondisk::Oid;
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::ops::Range;

struct Head<'a> {
    run: &'a Run,
    at: usize,
    end: usize,
    order: &'a dyn KeyOrder,
}

impl Head<'_> {
    fn cmp_entry(&self, other: &Self) -> Ordering {
        self.order
            .compare(self.run.key(self.at), other.run.key(other.at))
            .then(self.run.oid(self.at).cmp(&other.run.oid(other.at)))
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

pub struct KwayMerge<'a> {
    heap: BinaryHeap<Head<'a>>,
}

impl<'a> KwayMerge<'a> {
    pub fn new(runs: &'a [Run], ranges: &[Range<usize>], order: &'a dyn KeyOrder) -> Self {
        let heap = runs
            .iter()
            .zip(ranges)
            .filter(|(_, r)| !r.is_empty())
            .map(|(run, r)| Head {
                run,
                at: r.start,
                end: r.end,
                order,
            })
            .collect();
        Self { heap }
    }
}

impl<'a> Iterator for KwayMerge<'a> {
    type Item = (&'a [u8], Oid);

    fn next(&mut self) -> Option<Self::Item> {
        let mut top = self.heap.peek_mut()?;
        let item = (top.run.key(top.at), top.run.oid(top.at));
        top.at += 1;
        if top.at == top.end {
            std::collections::binary_heap::PeekMut::pop(top);
        }
        Some(item)
    }
}
