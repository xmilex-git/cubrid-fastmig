//! Creates the files of one B-tree in the target database before its keys exist: the B-tree file with its root,
//! and (on the first overflowing key) its overflow key file.
//!
//! Input: the space manager of the target, the index shape and the first key attribute. Output: the BTID (file,
//! root page) with an empty root written, so the BTID can go into the class object and the catalog before the
//! build fills the tree (`fill::fill_btrees`); the overflow key file's VFID.
//! Source: engine `btree_create_file` (btree.c, develop 682b080b2: `FILE_BTREE` of 1 page, descriptor
//! {class_oid, attr_ids[0]}, the root = sticky first page), `btree_create_overflow_key_file` (descriptor
//! {btid, class_oid}); R6 § 3. The empty root is `write_empty_root` (height 1, no key).

use std::io;

use fastmig_ondisk::btree::node_header::NodeHeader;
use fastmig_ondisk::btree::root_header::RootHeader;
use fastmig_ondisk::file_alloc::{FilePages, SpaceManager, perm_tablespace};
use fastmig_ondisk::file_header::{FILE_DESCRIPTORS_SIZE, FileType};
use fastmig_ondisk::page::PageType;
use fastmig_ondisk::{Btid, Oid, Vfid};

use crate::index_shape::IndexShape;
use crate::nonleaf_builder::write_empty_root;
use crate::volume_sink::VolumeFileSink;

/// The B-tree file of a new index and its empty root: the BTID is final from here on.
pub fn create_btree_file(
    space: &SpaceManager,
    shape: &IndexShape,
    attr_id: i32,
) -> io::Result<Btid> {
    let io_pagesize = space.volumes().io_pagesize();
    let mut des = [0u8; FILE_DESCRIPTORS_SIZE];
    shape.topclass.write(&mut des, 0);
    des[8..12].copy_from_slice(&attr_id.to_le_bytes());
    let vfid = space.create_file(FileType::Btree, des, perm_tablespace(io_pagesize, 1))?;
    let root = space.alloc_sticky_first_page(vfid, PageType::Btree)?;
    let mut header = empty_root_header(shape);
    write_empty_root(
        &VolumeFileSink::btree(FilePages { space, vfid }),
        root,
        &mut header,
        shape.db_pagesize(),
    )?;
    Ok(Btid {
        vfid,
        pageid: root.pageid,
    })
}

/// The overflow key file of `btid` (created only when a key overflows, as the engine does).
pub fn create_overflow_key_file(
    space: &SpaceManager,
    btid: Btid,
    class_oid: Oid,
) -> io::Result<Vfid> {
    let mut des = [0u8; FILE_DESCRIPTORS_SIZE];
    btid.write(&mut des, 0);
    class_oid.write(&mut des, 12);
    space.create_file(
        FileType::BtreeOverflowKey,
        des,
        perm_tablespace(space.volumes().io_pagesize(), 1),
    )
}

/// The root header of a tree with no key; `build_index` writes the real one over it.
fn empty_root_header(shape: &IndexShape) -> RootHeader {
    let stat = if shape.unique { 0 } else { -1 };
    RootHeader {
        node: NodeHeader::new(1),
        num_oids: stat,
        num_nulls: stat,
        num_keys: stat,
        topclass_oid: shape.topclass,
        unique_pk: shape.unique_pk(),
        off68: 0,
        ovfid: Vfid::NULL,
        creator_mvccid: 0,
        packed_key_domain: shape.packed_key_domain.clone(),
    }
}
