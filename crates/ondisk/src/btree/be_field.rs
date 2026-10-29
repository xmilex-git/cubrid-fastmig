//! Big-endian OID, VPID and MVCCID fields inside B-tree records.
//!
//! Input: an [`OrReader`] at the field (read), or the record being built (write). Output: the value, with the
//! flag bits the B-tree packs into an OID split off; or the field written.
//! Source: engine `OR_PUT_OID`/`OR_GET_OID`, `OR_PUT_INT`/`OR_PUT_SHORT`, `OR_PUT_MVCCID`
//! (object_representation.h, htonl/htons); flag masks `btree.c:113-130` (same in release/11.4 and develop).

use crate::ids::{Oid, Vpid};
use crate::value::{DecodeResult, OrReader};

/// Engine `BTREE_LEAF_RECORD_*`: leaf record flags in the top 4 bits of the first OID's slotid.
pub const LEAF_RECORD_FENCE: u16 = 0x1000;
pub const LEAF_RECORD_OVERFLOW_OIDS: u16 = 0x2000;
pub const LEAF_RECORD_OVERFLOW_KEY: u16 = 0x4000;
pub const LEAF_RECORD_CLASS_OID: u16 = 0x8000;
pub const LEAF_RECORD_MASK: u16 = 0xF000;

/// Engine `BTREE_OID_HAS_MVCC_*`: object MVCC flags in the top 2 bits of each object OID's volid.
pub const OID_HAS_MVCC_INSID: u16 = 0x4000;
pub const OID_HAS_MVCC_DELID: u16 = 0x8000;
pub const OID_MVCC_MASK: u16 = 0xC000;

/// An OID as written in a record, with the bits of slotid and volid that are flags, not identifier.
#[derive(Clone, Copy, Debug)]
pub struct FlaggedOid {
    pub oid: Oid,
    /// Top 4 bits of the slotid field (leaf record flags; meaningful on a record's first OID only).
    pub record_flags: u16,
    /// Top 2 bits of the volid field (object MVCC flags).
    pub mvcc_flags: u16,
}

pub fn read_flagged_oid(r: &mut OrReader) -> DecodeResult<FlaggedOid> {
    let pageid = r.i32()?;
    let slot = r.u16()?;
    let vol = r.u16()?;
    Ok(FlaggedOid {
        oid: Oid::new(
            (vol & !OID_MVCC_MASK) as i16,
            pageid,
            (slot & !LEAF_RECORD_MASK) as i16,
        ),
        record_flags: slot & LEAF_RECORD_MASK,
        mvcc_flags: vol & OID_MVCC_MASK,
    })
}

/// A plain OID (class OID): no flag bits.
pub fn read_oid(r: &mut OrReader) -> DecodeResult<Oid> {
    Ok(read_flagged_oid(r)?.oid)
}

pub fn put_flagged_oid(out: &mut Vec<u8>, oid: Oid, record_flags: u16, mvcc_flags: u16) {
    out.extend_from_slice(&oid.pageid.to_be_bytes());
    out.extend_from_slice(&((oid.slotid as u16) | record_flags).to_be_bytes());
    out.extend_from_slice(&((oid.volid as u16) | mvcc_flags).to_be_bytes());
}

pub fn read_mvccid(r: &mut OrReader) -> DecodeResult<u64> {
    Ok(u64::from_be_bytes(r.array()?))
}

pub fn put_mvccid(out: &mut Vec<u8>, id: u64) {
    out.extend_from_slice(&id.to_be_bytes());
}

/// `DISK_VPID_SIZE` form: int pageid + short volid, big-endian.
pub fn read_disk_vpid(r: &mut OrReader) -> DecodeResult<Vpid> {
    let pageid = r.i32()?;
    Ok(Vpid::new(r.i16()?, pageid))
}

pub fn put_disk_vpid(out: &mut Vec<u8>, v: Vpid) {
    out.extend_from_slice(&v.pageid.to_be_bytes());
    out.extend_from_slice(&v.volid.to_be_bytes());
}

/// Engine `or_put_align32`: zero bytes up to a multiple of 4.
pub fn pad_to_4(out: &mut Vec<u8>) {
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
}
