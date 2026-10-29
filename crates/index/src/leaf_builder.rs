//! Builds the leaf pages of one shard from its (key, OID) stream in index order.
//!
//! Input: the shard's merged stream. Output: leaf pages written to the sink except the shard's first and last
//! (kept for the seams), one summary per leaf for the non-leaf levels, and the shard's counts.
//! Source: engine `btree_construct_leafs`, `bt_load_make_new_record_on_leaf_page`, `btree_proceed_leaf`,
//! `btree_first_oid` (btree_load.c, develop 5b1867174); R6 § 4.3 R-LEAF-1..7, § 4.5 R-MVCC-1.
//! Rules: one record per key; a key image ≥ `max_keylen_inpage` goes to the overflow key file; objects past
//! `max_objects_in_leaf_record` go to overflow OID pages; a leaf takes no record that would leave it less free
//! space than `leaf_reserve` once it holds one key record.

use crate::btree_page::BtreePage;
use crate::build_error::BuildError;
use crate::index_shape::IndexShape;
use crate::key_order::KeyOrder;
use crate::kway_merge::KeyStream;
use crate::ovf_key_file::OvfKeyFile;
use crate::ovf_oid_chain::write_overflow_oids;
use crate::page_sink::PageSink;
use fastmig_ondisk::btree::DISK_VPID_SIZE;
use fastmig_ondisk::btree::leaf_record::{KeyRef, write_leaf_record};
use fastmig_ondisk::btree::node_header::NodeHeader;
use fastmig_ondisk::{Oid, Vpid};
use std::cmp::Ordering;

pub struct LeafPage {
    pub page: BtreePage,
    pub header: NodeHeader,
}

impl LeafPage {
    fn new(vpid: Vpid, db_pagesize: usize) -> Self {
        let mut page = BtreePage::new(vpid, db_pagesize);
        page.push(vec![0; NodeHeader::SIZE]);
        Self {
            page,
            header: NodeHeader::new(1),
        }
    }

    pub fn write(mut self, sink: &dyn PageSink) -> std::io::Result<()> {
        self.page.records[0] = self.header.to_bytes().to_vec();
        sink.write_page(self.page.vpid, &self.page.to_io_page(sink.io_pagesize()))
    }
}

/// A page of a level and the first key under it: what the level above needs.
pub struct ChildSummary {
    pub vpid: Vpid,
    pub first_key: Vec<u8>,
    pub max_key_len: i16,
}

/// Counts of one shard; summed over shards, with the seams corrected (`build`).
#[derive(Default)]
pub struct ShardCounts {
    pub keys: u64,
    pub oids: u64,
    pub leaf_pages: u64,
    pub ovf_oid_pages: u64,
    /// `pkeys[i]`: distinct prefixes of `i + 1` columns.
    pub pkeys: Vec<u64>,
}

pub struct ShardLeaves {
    pub leaves: Vec<ChildSummary>,
    /// The shard's first and last leaf, not yet written: their outer links are set at the seams.
    pub first: Option<LeafPage>,
    pub last: Option<LeafPage>,
    pub last_key: Vec<u8>,
    pub counts: ShardCounts,
}

struct Builder<'a, 'o> {
    shape: &'a IndexShape,
    order: &'a dyn KeyOrder,
    sink: &'a dyn PageSink,
    ovf_keys: &'a OvfKeyFile<'o>,
    out: ShardLeaves,
    current: Option<LeafPage>,
    rec: Vec<u8>,
}

impl Builder<'_, '_> {
    fn key_group(
        &mut self,
        key: &[u8],
        oids: &[Oid],
        prev_key: Option<&[u8]>,
    ) -> Result<(), BuildError> {
        let c = &mut self.out.counts;
        c.keys += 1;
        c.oids += oids.len() as u64;
        let common = prev_key.map_or(0, |p| self.order.common_columns(p, key));
        for n in &mut c.pkeys[common..] {
            *n += 1;
        }
        if self.shape.unique && oids.len() > 1 {
            return Err(BuildError::UniqueViolation {
                key: key.to_vec(),
                oids: oids.to_vec(),
            });
        }
        let (kref, stored_len) = if key.len() >= self.shape.max_keylen_inpage() {
            (
                KeyRef::Overflow(self.ovf_keys.write_key(key)?),
                DISK_VPID_SIZE,
            )
        } else {
            (KeyRef::Inline(key), key.len())
        };
        let max = self.shape.max_objects_in_leaf_record();
        let ovf = if oids.len() > max {
            let (first, pages) = write_overflow_oids(self.sink, &oids[max..])?;
            self.out.counts.ovf_oid_pages += pages as u64;
            Some(first)
        } else {
            None
        };
        let in_record = &oids[..oids.len().min(max)];
        write_leaf_record(
            &mut self.rec,
            self.shape.unique,
            self.shape.topclass,
            in_record,
            kref,
            ovf,
        );
        self.place(key, stored_len)
    }

    fn place(&mut self, key: &[u8], stored_len: usize) -> Result<(), BuildError> {
        let need_new = match &self.current {
            None => true,
            Some(cur) => {
                let left = cur.page.max_space_for_new_record() as f64 - self.rec.len() as f64;
                (cur.page.key_records() >= 1 && left < self.shape.leaf_reserve())
                    || !cur.page.fits(self.rec.len())
            }
        };
        if need_new {
            let vpid = self.sink.alloc_page()?;
            let mut next = LeafPage::new(vpid, self.shape.db_pagesize());
            if let Some(mut cur) = self.current.take() {
                cur.header.next = vpid;
                next.header.prev = cur.page.vpid;
                if self.out.first.is_none() {
                    self.out.first = Some(cur);
                } else {
                    cur.write(self.sink)?;
                }
            }
            self.out.counts.leaf_pages += 1;
            self.out.leaves.push(ChildSummary {
                vpid,
                first_key: key.to_vec(),
                max_key_len: 0,
            });
            self.current = Some(next);
        }
        let cur = self.current.as_mut().unwrap();
        cur.page.push(std::mem::take(&mut self.rec));
        cur.header.max_key_len = cur.header.max_key_len.max(stored_len as i16);
        let summary = self.out.leaves.last_mut().unwrap();
        summary.max_key_len = cur.header.max_key_len;
        Ok(())
    }
}

/// Builds one shard. `stream` yields (key, OID) in key then OID order.
pub fn build_shard(
    shape: &IndexShape,
    order: &dyn KeyOrder,
    sink: &dyn PageSink,
    ovf_keys: &OvfKeyFile,
    stream: &mut dyn KeyStream,
) -> Result<ShardLeaves, BuildError> {
    let mut b = Builder {
        shape,
        order,
        sink,
        ovf_keys,
        out: ShardLeaves {
            leaves: Vec::new(),
            first: None,
            last: None,
            last_key: Vec::new(),
            counts: ShardCounts {
                pkeys: vec![0; order.columns()],
                ..Default::default()
            },
        },
        current: None,
        rec: Vec::new(),
    };
    let mut prev_key: Option<Vec<u8>> = None;
    let mut key: Vec<u8> = Vec::new();
    let mut oids: Vec<Oid> = Vec::new();
    while let Some((k, oid)) = stream.next_pair()? {
        if !oids.is_empty() {
            match order.compare(k, &key) {
                Ordering::Equal => {
                    if oids.last() == Some(&oid) {
                        return Err(BuildError::DuplicateEntry {
                            key: key.clone(),
                            oid,
                        });
                    }
                    oids.push(oid);
                    continue;
                }
                Ordering::Greater => {
                    b.key_group(&key, &oids, prev_key.as_deref())?;
                    prev_key = Some(std::mem::take(&mut key));
                    oids.clear();
                }
                Ordering::Less => unreachable!("stream out of order"),
            }
        }
        key.clear();
        key.extend_from_slice(k);
        oids.push(oid);
    }
    if !oids.is_empty() {
        b.key_group(&key, &oids, prev_key.as_deref())?;
    }
    let mut out = b.out;
    if let Some(cur) = b.current {
        if out.first.is_none() {
            out.first = Some(cur);
        } else {
            out.last = Some(cur);
        }
    }
    out.last_key = key;
    Ok(out)
}
