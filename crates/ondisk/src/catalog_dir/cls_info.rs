//! The class-info record of one class: heap, page and row counts, and the back link to its directory.
//!
//! Input: the 56-byte record and the release line. Output: [`ClsInfo`], or its develop bytes.
//! Source: engine `system_catalog.c` `CATALOG_CLS_INFO_*_OFF`: HFID @0 (12 B), tot_pages @12, tot_objs @16 (11.4
//! row count; develop writes 0 and does not read it), time_stamp @20, rep_dir OID @24, develop tot_objs INT64 @32
//! (519cc1a43, PR #7856), zero to 56; release/11.4 @46212adcd, develop @5b1867174; R3 § 3, rule E3.

use super::obj_ids::{read_hfid, read_oid, write_hfid, write_oid};
use crate::value::{DecodeResult, OrReader, OrWriter};
use crate::{Hfid, Oid, VersionLine};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClsInfo {
    pub hfid: Hfid,
    pub tot_pages: i32,
    /// Row count, widened from the 11.4 int.
    pub tot_objs: i64,
    pub time_stamp: i32,
    pub rep_dir: Oid,
}

impl ClsInfo {
    pub const SIZE: usize = 56;

    pub fn decode(rec: &[u8], line: VersionLine) -> DecodeResult<Self> {
        let mut r = OrReader::new(rec);
        let hfid = read_hfid(&mut r)?;
        let tot_pages = r.i32()?;
        let objs32 = r.i32()?;
        let time_stamp = r.i32()?;
        let rep_dir = read_oid(&mut r)?;
        let tot_objs = match line {
            VersionLine::V11_4 => objs32 as i64,
            VersionLine::Develop => r.i64()?,
        };
        Ok(ClsInfo {
            hfid,
            tot_pages,
            tot_objs,
            time_stamp,
            rep_dir,
        })
    }

    /// The develop record (`catalog_put_class_info_to_record`).
    pub fn encode_develop(&self) -> Vec<u8> {
        let mut w = OrWriter::new();
        write_hfid(&mut w, self.hfid);
        w.i32(self.tot_pages);
        w.i32(0);
        w.i32(self.time_stamp);
        write_oid(&mut w, self.rep_dir);
        w.i64(self.tot_objs);
        w.zeros(Self::SIZE - w.len());
        w.buf
    }
}
