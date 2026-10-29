//! The header record (slot 0) of every catalog page: overflow link, directory count, overflow flag.
//!
//! Input: the slot-0 record. Output: [`CatalogPageHeader`], or its 16 bytes.
//! Source: engine `system_catalog.c` `CATALOG_PGHEADER_*_OFF` (ovfl pageid @0 int, ovfl volid @4 short,
//! dir_count @8 int, is_overflow @12 int; 16 B), `catalog_initialize_new_page` (`PAGE_CATALOG`,
//! `ANCHORED_DONT_REUSE_SLOTS`, `MAX_ALIGNMENT`), `catalog_get_record_from_page` (a record continues in slot 1 of
//! the page the header links to); same in release/11.4 @46212adcd and develop @5b1867174; R3 § 3.

use crate::Vpid;
use crate::value::{DecodeResult, OrReader, OrWriter};

/// Engine `CATALOG_HEADER_SLOT`.
pub const CATALOG_HEADER_SLOT: usize = 0;
/// Engine `CATALOG_MAX_SLOT_ID_SIZE`: the room a fragment leaves for slot growth.
pub const CATALOG_MAX_SLOT_ID_SIZE: usize = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CatalogPageHeader {
    /// Page holding the continuation of this page's last record; NULL when none.
    pub overflow: Vpid,
    /// Directory records on this page.
    pub dir_count: i32,
    /// The page only holds a continuation fragment.
    pub is_overflow: bool,
}

impl CatalogPageHeader {
    pub const SIZE: usize = 16;

    pub fn decode(rec: &[u8]) -> DecodeResult<Self> {
        let mut r = OrReader::new(rec);
        let pageid = r.i32()?;
        let volid = r.i16()?;
        r.skip(2)?;
        Ok(CatalogPageHeader {
            overflow: Vpid::new(volid, pageid),
            dir_count: r.i32()?,
            is_overflow: r.i32()? != 0,
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut w = OrWriter::new();
        w.i32(self.overflow.pageid);
        w.i16(self.overflow.volid);
        w.zeros(2);
        w.i32(self.dir_count);
        w.i32(self.is_overflow as i32);
        w.buf
    }
}
