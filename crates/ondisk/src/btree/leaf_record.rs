//! Leaf record: one key and the objects (row OIDs) that carry it.
//!
//! Input: a leaf record (slot ≥ 1 of a leaf page) and the key's `KeyImage`. Output: the parsed record;
//! or, for the builder, the record bytes for a key and its objects (all rows visible to everyone).
//! Source: engine `btree_write_record_header`, `btree_write_record_ex`, `btree_leaf_record_handle_first_overflow`
//! (btree.c), `btree_first_oid`, `bt_load_add_same_key_to_record` (btree_load.c), develop 5b1867174; the
//! same in release/11.4 46212adcd. R6 § 3.3.
//!
//! Layout: first object (OID with record flags in its slotid, MVCC flags in its volid; [class OID] if
//! `CLASS_OID`; [insert MVCCID]; [delete MVCCID]) · key image, or overflow key VPID (6 B) if `OVERFLOW_KEY`
//! · zero pad to 4 · further objects (OID; class OID always in a unique index; MVCCIDs per flags) ·
//! overflow OID page VPID (8 B) if `OVERFLOW_OIDS`.

use super::be_field::*;
use super::key_image::KeyImage;
use super::{DISK_VPID_ALIGNED_SIZE, DISK_VPID_SIZE, MVCCID_ALL_VISIBLE};
use crate::ids::{Oid, Vpid};

/// The key of a leaf or non-leaf record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyRef<'a> {
    /// The key image itself.
    Inline(&'a [u8]),
    /// First page of the key's chain in the overflow key file.
    Overflow(Vpid),
}

/// One object of a leaf record or overflow OID page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BtObject {
    pub oid: Oid,
    pub class_oid: Option<Oid>,
    pub insid: Option<u64>,
    pub delid: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct LeafRecord<'a> {
    /// `LEAF_RECORD_*` flags of the record.
    pub flags: u16,
    pub first: BtObject,
    pub key: KeyRef<'a>,
    /// Objects after the key, in record order.
    pub others: Vec<BtObject>,
    /// First overflow OID page when `OVERFLOW_OIDS`.
    pub ovf_oids: Option<Vpid>,
}

impl LeafRecord<'_> {
    pub fn is_fence(&self) -> bool {
        self.flags & LEAF_RECORD_FENCE != 0
    }
}

/// Reads one object at `off`; `class_oid` says whether a class OID follows the OID.
/// Returns the object and the offset after it.
pub fn read_object(
    b: &[u8],
    off: usize,
    class_oid: bool,
) -> Result<(BtObject, usize), &'static str> {
    let need = |end: usize| {
        if end <= b.len() {
            Ok(())
        } else {
            Err("object runs past the record")
        }
    };
    need(off + 8)?;
    let f = get_flagged_oid(b, off);
    let mut p = off + 8;
    let class = if class_oid {
        need(p + 8)?;
        p += 8;
        Some(get_oid(b, p - 8))
    } else {
        None
    };
    let mut mvcc = |flag: u16| -> Result<Option<u64>, &'static str> {
        if f.mvcc_flags & flag == 0 {
            return Ok(None);
        }
        need(p + 8)?;
        p += 8;
        Ok(Some(get_mvccid(b, p - 8)))
    };
    let insid = mvcc(OID_HAS_MVCC_INSID)?;
    let delid = mvcc(OID_HAS_MVCC_DELID)?;
    Ok((
        BtObject {
            oid: f.oid,
            class_oid: class,
            insid,
            delid,
        },
        p,
    ))
}

/// Parses a leaf record of an index whose uniqueness is `unique`. A fence record's key is returned as it is
/// stored; a key on a prefix-compressed page is the stored (shortened) image.
pub fn read_leaf_record<'a>(
    b: &'a [u8],
    unique: bool,
    keys: &dyn KeyImage,
) -> Result<LeafRecord<'a>, &'static str> {
    if b.len() < 8 {
        return Err("leaf record shorter than an OID");
    }
    let first_oid = get_flagged_oid(b, 0);
    let flags = first_oid.record_flags;
    let (first, mut p) = read_object(b, 0, flags & LEAF_RECORD_CLASS_OID != 0)?;
    let end = if flags & LEAF_RECORD_OVERFLOW_OIDS != 0 {
        b.len()
            .checked_sub(DISK_VPID_ALIGNED_SIZE)
            .ok_or("no room for the overflow OID link")?
    } else {
        b.len()
    };
    if p > end {
        return Err("first object runs into the overflow OID link");
    }
    let key = if flags & LEAF_RECORD_OVERFLOW_KEY != 0 {
        if p + DISK_VPID_SIZE > end {
            return Err("overflow key link runs past the record");
        }
        p += DISK_VPID_SIZE;
        KeyRef::Overflow(get_disk_vpid(b, p - DISK_VPID_SIZE))
    } else {
        let n = keys.image_len(&b[p..end]);
        if p + n > end {
            return Err("key image runs past the record");
        }
        p += n;
        KeyRef::Inline(&b[p - n..p])
    };
    p = p.div_ceil(4) * 4;
    let mut others = Vec::new();
    while p < end {
        let (o, next) = read_object(&b[..end], p, unique)?;
        others.push(o);
        p = next;
    }
    let ovf_oids = (flags & LEAF_RECORD_OVERFLOW_OIDS != 0).then(|| get_disk_vpid(b, end));
    Ok(LeafRecord {
        flags,
        first,
        key,
        others,
        ovf_oids,
    })
}

/// Appends one fixed-size object (both MVCC flags, insert MVCCID all-visible, delete MVCCID 0): the form of
/// overflow page objects, of a unique index's non-first objects, and of a first object with overflow OIDs.
pub fn put_fixed_object(out: &mut Vec<u8>, oid: Oid, class_oid: Option<Oid>, record_flags: u16) {
    put_flagged_oid(
        out,
        oid,
        record_flags,
        OID_HAS_MVCC_INSID | OID_HAS_MVCC_DELID,
    );
    if let Some(c) = class_oid {
        put_flagged_oid(out, c, 0, 0);
    }
    put_mvccid(out, MVCCID_ALL_VISIBLE);
    put_mvccid(out, 0);
}

/// Builds the leaf record of `key` holding `oids` (ascending), every row visible to all (R6 R-MVCC-1).
/// `ovf_oids` is the first overflow OID page when the key has more objects than fit in the record; the first
/// object is then fixed size (and, in a unique index, carries `topclass` as its class OID).
/// `key` is an inline image or the overflow key VPID already written.
pub fn write_leaf_record(
    out: &mut Vec<u8>,
    unique: bool,
    topclass: Oid,
    oids: &[Oid],
    key: KeyRef,
    ovf_oids: Option<Vpid>,
) {
    out.clear();
    let mut flags = 0u16;
    if matches!(key, KeyRef::Overflow(_)) {
        flags |= LEAF_RECORD_OVERFLOW_KEY;
    }
    if ovf_oids.is_some() {
        flags |= LEAF_RECORD_OVERFLOW_OIDS;
        let class = unique.then(|| {
            flags |= LEAF_RECORD_CLASS_OID;
            topclass
        });
        put_fixed_object(out, oids[0], class, flags);
    } else {
        // all-visible first object of the indexed class: OID only (btree_write_record_header)
        put_flagged_oid(out, oids[0], flags, 0);
    }
    match key {
        KeyRef::Inline(img) => out.extend_from_slice(img),
        KeyRef::Overflow(v) => put_disk_vpid(out, v),
    }
    pad_to_4(out);
    for &oid in &oids[1..] {
        if unique {
            put_fixed_object(out, oid, Some(topclass), 0);
        } else {
            put_flagged_oid(out, oid, 0, 0);
        }
    }
    if let Some(v) = ovf_oids {
        put_disk_vpid(out, v);
        out.extend_from_slice(&[0, 0]);
    }
}
