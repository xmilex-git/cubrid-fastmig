//! Writes the overflow OID pages of one key whose objects do not fit in its leaf record, and the OID
//! directory over them.
//!
//! Input: the objects past the leaf record (OID ascending). Output: the chain's first VPID and the pages written.
//! Source: engine `bt_load_nospace_for_new_oid`, `bt_load_add_same_key_to_record`, `bt_load_finalize_ovf_dir`
//! (btree_load.c, develop 5b1867174); R6 § 3.5, R-LEAF-3. A page holds as many fixed-size objects as
//! `spage_max_space_for_new_record` leaves after its header. Non-unique only: a unique key with two rows is a
//! violation before it gets here.

use crate::btree_page::BtreePage;
use crate::page_sink::PageSink;
use fastmig_ondisk::btree::leaf_record::put_fixed_object;
use fastmig_ondisk::btree::ovf_oid_page::{OvfDirEntry, OvfHeader, dir_max_entries};
use fastmig_ondisk::overflow_part::OverflowChain;
use fastmig_ondisk::{Oid, Vpid, db_pagesize};
use std::io;

/// Objects one data page of a non-unique chain holds (679 at 16 KiB).
fn objects_per_page(db: usize) -> usize {
    let mut p = BtreePage::new(Vpid::NULL, db);
    p.push(vec![0; OvfHeader::DIR_SIZE]);
    p.max_space_for_new_record() / fastmig_ondisk::btree::object_fixed_size(false)
}

/// Writes the chain; its pages count data and directory pages.
pub fn write_overflow_oids(sink: &dyn PageSink, oids: &[Oid]) -> io::Result<OverflowChain> {
    let io_size = sink.io_pagesize();
    let db = db_pagesize(io_size);
    let chunks: Vec<&[Oid]> = oids.chunks(objects_per_page(db)).collect();
    let data: Vec<Vpid> = chunks
        .iter()
        .map(|_| sink.alloc_page())
        .collect::<io::Result<_>>()?;
    // a directory exists once the chain has two data pages (bt_load_nospace_for_new_oid)
    let dir_pages: Vec<Vpid> = if data.len() >= 2 {
        let n = data.len().div_ceil(dir_max_entries(db));
        (0..n)
            .map(|_| sink.alloc_page())
            .collect::<io::Result<_>>()?
    } else {
        Vec::new()
    };
    let dir_head = dir_pages.first().copied().unwrap_or(Vpid::NULL);
    for (i, (&v, chunk)) in data.iter().zip(&chunks).enumerate() {
        let mut page = BtreePage::new(v, db);
        let next = data.get(i + 1).copied().unwrap_or(Vpid::NULL);
        page.push(
            OvfHeader {
                next,
                dir: Some(dir_head),
            }
            .to_bytes(),
        );
        let mut rec = Vec::with_capacity(chunk.len() * 24);
        for &oid in *chunk {
            put_fixed_object(&mut rec, oid, None, 0);
        }
        page.push(rec);
        sink.write_page(v, &page.to_io_page(io_size))?;
    }
    let entries: Vec<OvfDirEntry> = data
        .iter()
        .zip(&chunks)
        .map(|(&vpid, c)| OvfDirEntry {
            sep_oid: c[0],
            vpid,
        })
        .collect();
    for (i, (&v, part)) in dir_pages
        .iter()
        .zip(entries.chunks(dir_max_entries(db)))
        .enumerate()
    {
        let mut page = BtreePage::new(v, db);
        let next = dir_pages.get(i + 1).copied().unwrap_or(Vpid::NULL);
        page.push(
            OvfHeader {
                next,
                dir: Some(Vpid::NULL),
            }
            .to_bytes(),
        );
        let mut rec = Vec::with_capacity(part.len() * OvfDirEntry::SIZE);
        for e in part {
            e.write(&mut rec);
        }
        page.push(rec);
        sink.write_page(v, &page.to_io_page(io_size))?;
    }
    Ok(OverflowChain {
        first: data[0],
        pages: data.len() + dir_pages.len(),
    })
}
