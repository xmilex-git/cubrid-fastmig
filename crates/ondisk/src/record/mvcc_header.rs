//! The header at the start of every heap record: representation id word, CHN, and the optional MVCC fields.
//!
//! Input: a whole record. Output: [`RecordHeader`] (repid, bound-bit flag, variable offset size, MVCC flags, CHN,
//! insert/delete MVCCID, previous-version LSA bytes) and its length; or the header bytes.
//! Source: engine `src/base/object_representation_constants.h:140-173` (flags, masks; diff 0 between
//! release/11.4 @46212adcd and develop @5b1867174), `src/base/object_representation.h:530-616` (offsets, BE
//! integers), `object_representation_sr.c:4707-4721` (prev LSA copied in native order); R2 § 1.6.

use crate::value::or_buf::{DecodeResult, OrReader, OrWriter};

/// Engine `OR_BOUND_BIT_FLAG`: the record has a bound-bit array after its fixed area.
const BOUND_BIT_FLAG: u32 = 0x8000_0000;
/// Engine `OR_OFFSET_SIZE_FLAG`; `01` 1 B, `10` 2 B, `11` 4 B variable offsets.
const OFFSET_SIZE_FLAG: u32 = 0x6000_0000;
const OFFSET_SIZE_SHIFT: u32 = 29;
const MVCC_FLAG_SHIFT: u32 = 24;
const MVCC_FLAG_MASK: u32 = 0x1F;
const REPID_MASK: u32 = 0x00FF_FFFF;

/// Engine `OR_MVCC_FLAG_VALID_INSID`.
pub const FLAG_INSID: u8 = 0x01;
/// Engine `OR_MVCC_FLAG_VALID_DELID`.
pub const FLAG_DELID: u8 = 0x02;
/// Engine `OR_MVCC_FLAG_VALID_PREV_VERSION`.
pub const FLAG_PREV_VERSION: u8 = 0x04;

/// A decoded record header. Layout: repid word BE @0, CHN BE @4, then INSID BE u64 if [`FLAG_INSID`], DELID BE u64
/// if [`FLAG_DELID`], prev-version LSA (8 native bytes) if [`FLAG_PREV_VERSION`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordHeader {
    pub repid: u32,
    pub has_bound_bits: bool,
    /// Bytes per variable offset entry: 1, 2 or 4.
    pub offset_size: usize,
    /// All five MVCC flag bits as stored.
    pub mvcc_flags: u8,
    pub chn: i32,
    pub insid: Option<u64>,
    pub delid: Option<u64>,
    pub prev_version_lsa: Option<[u8; 8]>,
}

impl RecordHeader {
    pub fn read(rec: &[u8]) -> DecodeResult<RecordHeader> {
        let mut r = OrReader::new(rec);
        let word = r.u32()?;
        let chn = r.i32()?;
        let mvcc_flags = ((word >> MVCC_FLAG_SHIFT) & MVCC_FLAG_MASK) as u8;
        let insid = if mvcc_flags & FLAG_INSID != 0 {
            Some(r.i64()? as u64)
        } else {
            None
        };
        let delid = if mvcc_flags & FLAG_DELID != 0 {
            Some(r.i64()? as u64)
        } else {
            None
        };
        let prev_version_lsa = if mvcc_flags & FLAG_PREV_VERSION != 0 {
            Some(r.bytes(8)?.try_into().unwrap())
        } else {
            None
        };
        let offset_size = match (word & OFFSET_SIZE_FLAG) >> OFFSET_SIZE_SHIFT {
            1 => 1,
            2 => 2,
            _ => 4,
        };
        Ok(RecordHeader {
            repid: word & REPID_MASK,
            has_bound_bits: word & BOUND_BIT_FLAG != 0,
            offset_size,
            mvcc_flags,
            chn,
            insid,
            delid,
            prev_version_lsa,
        })
    }

    /// Header length: 8 plus 8 per MVCC field present (engine `mvcc_header_size_lookup`).
    pub fn size(&self) -> usize {
        8 + 8
            * (self.insid.is_some() as usize
                + self.delid.is_some() as usize
                + self.prev_version_lsa.is_some() as usize)
    }

    /// A committed delete (engine `MVCC_IS_HEADER_DELID_VALID`): the source is shut down cleanly, so every
    /// non-zero DELID is committed (R2 § 3 R3).
    pub fn is_deleted(&self) -> bool {
        self.delid.is_some_and(|d| d != 0)
    }

    /// The repid word with flags as stored (MVCC flag bits recomputed from the present fields plus any others kept).
    pub fn word(&self) -> u32 {
        let mut flags = self.mvcc_flags & !(FLAG_INSID | FLAG_DELID | FLAG_PREV_VERSION);
        flags |= if self.insid.is_some() { FLAG_INSID } else { 0 }
            | if self.delid.is_some() { FLAG_DELID } else { 0 }
            | if self.prev_version_lsa.is_some() {
                FLAG_PREV_VERSION
            } else {
                0
            };
        let size_bits = match self.offset_size {
            1 => 1,
            2 => 2,
            _ => 3,
        };
        (self.repid & REPID_MASK)
            | ((flags as u32) << MVCC_FLAG_SHIFT)
            | (size_bits << OFFSET_SIZE_SHIFT)
            | if self.has_bound_bits {
                BOUND_BIT_FLAG
            } else {
                0
            }
    }

    pub fn write(&self, w: &mut OrWriter) {
        w.u32(self.word());
        w.i32(self.chn);
        if let Some(v) = self.insid {
            w.i64(v as i64);
        }
        if let Some(v) = self.delid {
            w.i64(v as i64);
        }
        if let Some(l) = self.prev_version_lsa {
            w.bytes(&l);
        }
    }
}
