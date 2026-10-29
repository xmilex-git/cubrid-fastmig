//! Builds the non-leaf levels over the leaves and writes the root.
//!
//! Input: the leaves in order with their first keys. Output: non-leaf pages and the root page (the B-tree
//! file's sticky first page); the tree height and page count.
//! Source: engine `btree_build_nleafs` PHASE I–III, `btree_connect_page` (btree_load.c, develop 5b1867174);
//! R6 § 4.4 R-NL-1..4, R-ROOT-1..2. Separator key = the right child's first key, whole (workspace#483 G3; the
//! engine's shortest prefix is a later optimisation). A level's pages get their VPIDs when the level is done,
//! so the top level goes straight to the root page.

use crate::btree_page::BtreePage;
use crate::index_shape::IndexShape;
use crate::leaf_builder::ChildSummary;
use crate::ovf_key_file::OvfKeyFile;
use crate::page_sink::PageSink;
use fastmig_ondisk::Vpid;
use fastmig_ondisk::btree::DISK_VPID_SIZE;
use fastmig_ondisk::btree::leaf_record::KeyRef;
use fastmig_ondisk::btree::node_header::NodeHeader;
use fastmig_ondisk::btree::nonleaf_record::write_nonleaf_record;
use fastmig_ondisk::btree::root_header::RootHeader;
use std::io;

struct Pending {
    page: BtreePage,
    first_key: Vec<u8>,
    max_key_len: i16,
}

/// Builds every non-leaf level and writes the root at `root`. `level` (the leaves) is non-empty. Fills
/// `root_header`'s node header and `ovfid`. Returns (height, non-leaf pages including the root).
pub fn build_nonleaf_levels(
    shape: &IndexShape,
    sink: &dyn PageSink,
    ovf_keys: &OvfKeyFile,
    root: Vpid,
    mut level: Vec<ChildSummary>,
    root_header: &mut RootHeader,
) -> io::Result<(i16, u64)> {
    let db = shape.db_pagesize();
    let io_size = sink.io_pagesize();
    let mut node_level = 2i16;
    let mut pages_written = 0u64;
    let mut rec = Vec::new();
    loop {
        let mut pages: Vec<Pending> = Vec::new();
        for child in &level {
            let (kref, stored) = if child.first_key.len() >= shape.max_keylen_inpage() {
                (
                    KeyRef::Overflow(ovf_keys.write_key(&child.first_key)?),
                    DISK_VPID_SIZE,
                )
            } else {
                (KeyRef::Inline(&child.first_key[..]), child.first_key.len())
            };
            write_nonleaf_record(&mut rec, child.vpid, kref);
            let full = pages.last().is_none_or(|p| {
                let left = p.page.max_space_for_new_record() as f64 - rec.len() as f64;
                (p.page.key_records() >= 1 && left < shape.nonleaf_reserve())
                    || !p.page.fits(rec.len())
            });
            if full {
                let mut page = BtreePage::new(Vpid::NULL, db);
                page.push(vec![0; NodeHeader::SIZE]);
                pages.push(Pending {
                    page,
                    first_key: child.first_key.clone(),
                    max_key_len: 0,
                });
            }
            let p = pages.last_mut().unwrap();
            p.page.push(rec.clone());
            // subtree maximum: the child's and this separator's
            p.max_key_len = p.max_key_len.max(child.max_key_len).max(stored as i16);
        }
        if pages.len() == 1 {
            let p = pages.pop().unwrap();
            let mut page = p.page;
            page.vpid = root;
            root_header.node = NodeHeader {
                max_key_len: p.max_key_len,
                ..NodeHeader::new(node_level)
            };
            root_header.ovfid = ovf_keys.vfid().unwrap_or(fastmig_ondisk::Vfid::NULL);
            page.records[0] = root_header.to_bytes();
            sink.write_page(root, &page.to_io_page(io_size))?;
            return Ok((node_level, pages_written + 1));
        }
        let mut next = Vec::with_capacity(pages.len());
        for mut p in pages {
            p.page.vpid = sink.alloc_page()?;
            p.page.records[0] = NodeHeader {
                max_key_len: p.max_key_len,
                ..NodeHeader::new(node_level)
            }
            .to_bytes()
            .to_vec();
            sink.write_page(p.page.vpid, &p.page.to_io_page(io_size))?;
            pages_written += 1;
            next.push(ChildSummary {
                vpid: p.page.vpid,
                first_key: p.first_key,
                max_key_len: p.max_key_len,
            });
        }
        level = next;
        node_level += 1;
    }
}

/// The root of an index without any key: a leaf-level root holding only the root header (engine
/// `xbtree_add_index` → `btree_create` for an empty build).
pub fn write_empty_root(
    sink: &dyn PageSink,
    root: Vpid,
    root_header: &mut RootHeader,
    db: usize,
) -> io::Result<()> {
    root_header.node = NodeHeader::new(1);
    let mut page = BtreePage::new(root, db);
    page.push(root_header.to_bytes());
    sink.write_page(root, &page.to_io_page(sink.io_pagesize()))
}
