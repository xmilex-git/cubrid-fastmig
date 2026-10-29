//! The directory record of one class: where its class-info record and its last disk representation live.
//!
//! Input: the directory record (the OID a class object's `rep_dir` names). Output: [`ReprDir`], or its bytes.
//! Source: engine `system_catalog.c` `CATALOG_REPR_ITEM_*_OFF` (pageid @0 int, volid @4 short, repr_id @8 short,
//! slotid @10 short, count @12 byte in item 0; 16 B per item), `catalog_put_representation_item` (item 0 is the
//! class info with `NULL_REPRID`, the count is the number of items); same in release/11.4 @46212adcd and
//! develop @5b1867174; R3 § 3, rule E4.

use crate::value::{DecodeResult, OrReader, OrWriter};
use crate::{Oid, Vpid};

/// Engine `NULL_REPRID`: the repr id of the class-info item.
pub const NULL_REPRID: i16 = -1;
const ITEM_SIZE: usize = 16;

/// One item: the record a repr id names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReprItem {
    pub repr_id: i16,
    pub record: Oid,
}

/// The directory record. In both lines the engine keeps two items: class info, then the current representation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReprDir {
    pub items: Vec<ReprItem>,
}

impl ReprDir {
    pub fn decode(rec: &[u8]) -> DecodeResult<Self> {
        let mut r = OrReader::new(rec);
        let mut items = Vec::new();
        let mut count = 1;
        while items.len() < count {
            let at = items.len() * ITEM_SIZE;
            r.skip(at - r.pos())?;
            let pageid = r.i32()?;
            let volid = r.i16()?;
            r.skip(2)?;
            let repr_id = r.i16()?;
            let slotid = r.i16()?;
            if items.is_empty() {
                count = r.u8()? as usize;
                if count * ITEM_SIZE > rec.len() {
                    return Err(
                        r.error(format!("{count} items do not fit a {} B record", rec.len()))
                    );
                }
            }
            let v = Vpid::new(volid, pageid);
            items.push(ReprItem {
                repr_id,
                record: Oid::new(v.volid, v.pageid, slotid),
            });
        }
        Ok(ReprDir { items })
    }

    /// The class-info record (item with `NULL_REPRID`).
    pub fn class_info(&self) -> Option<Oid> {
        self.items
            .iter()
            .find(|i| i.repr_id == NULL_REPRID)
            .map(|i| i.record)
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut w = OrWriter::new();
        for (n, i) in self.items.iter().enumerate() {
            w.i32(i.record.pageid);
            w.i16(i.record.volid);
            w.zeros(2);
            w.i16(i.repr_id);
            w.i16(i.record.slotid);
            w.u8(if n == 0 { self.items.len() as u8 } else { 0 });
            w.zeros(3);
        }
        w.buf
    }
}
