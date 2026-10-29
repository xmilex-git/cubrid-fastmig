//! OID, HFID and BTID as catalog records store them (big endian, `OR_PUT_*` order), which differs from the
//! little-endian structs in page headers (`crate::ids`).
//!
//! Input: an OR reader or writer. Output: the identifier.
//! Source: engine `object_representation.h` `OR_PUT_OID` (pageid, slotid, volid), `OR_PUT_HFID` (hpgid, fileid,
//! volid as ints, 12 B), `OR_PUT_BTID` (root pageid, fileid ints, volid short, `OR_BTID_ALIGNED_SIZE` 12 B with
//! 2 B pad); `object_representation_constants.h:77-85`; same in release/11.4 and develop.

use crate::value::{DecodeResult, OrReader, OrWriter};
use crate::{Btid, Hfid, Oid, Vfid};

pub(crate) fn read_oid(r: &mut OrReader) -> DecodeResult<Oid> {
    let pageid = r.i32()?;
    let slotid = r.i16()?;
    let volid = r.i16()?;
    Ok(Oid::new(volid, pageid, slotid))
}

pub(crate) fn write_oid(w: &mut OrWriter, o: Oid) {
    w.i32(o.pageid);
    w.i16(o.slotid);
    w.i16(o.volid);
}

pub(crate) fn read_hfid(r: &mut OrReader) -> DecodeResult<Hfid> {
    let pageid = r.i32()?;
    let fileid = r.i32()?;
    let volid = r.i32()?;
    Ok(Hfid {
        vfid: Vfid::new(volid as i16, fileid),
        pageid,
    })
}

pub(crate) fn write_hfid(w: &mut OrWriter, h: Hfid) {
    w.i32(h.pageid);
    w.i32(h.vfid.fileid);
    w.i32(h.vfid.volid as i32);
}

/// Reads the 12 aligned bytes.
pub(crate) fn read_btid(r: &mut OrReader) -> DecodeResult<Btid> {
    let pageid = r.i32()?;
    let fileid = r.i32()?;
    let volid = r.i16()?;
    r.skip(2)?;
    Ok(Btid {
        vfid: Vfid::new(volid, fileid),
        pageid,
    })
}

pub(crate) fn write_btid(w: &mut OrWriter, b: Btid) {
    w.i32(b.pageid);
    w.i32(b.vfid.fileid);
    w.i16(b.vfid.volid);
    w.zeros(2);
}
