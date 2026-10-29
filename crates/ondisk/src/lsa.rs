//! Log sequence address (LSA): a position in the log, stored in every page header and in the log header.
//!
//! Input: the 8-byte encoded LSA. Output: (pageid, offset), or the 8 bytes written back.
//! Source: engine `src/transaction/log_lsa.hpp:36-40` (`log_lsa`: `int64 pageid:48; int64 offset:16`, a bit field in
//! one little-endian int64, pageid in the low 48 bits); `NULL_LSA` = pageid -1, offset -1 (all bytes 0xFF).
//! Same in release/11.4 @46212adcd and develop @5b1867174; checked on T1 page headers (workspace#490).

use zerocopy::little_endian::U64;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use crate::le_field::{get_u64, put_u64};

/// Engine `LOG_LSA`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Lsa {
    /// Log page id, 48-bit signed.
    pub pageid: i64,
    /// Byte offset in the log page, 16-bit signed.
    pub offset: i16,
}

impl Lsa {
    pub const SIZE: usize = 8;
    pub const NULL: Lsa = Lsa {
        pageid: -1,
        offset: -1,
    };

    pub fn new(pageid: i64, offset: i16) -> Self {
        Lsa { pageid, offset }
    }

    pub fn is_null(&self) -> bool {
        self.pageid == -1
    }

    pub fn from_u64(raw: u64) -> Self {
        // sign-extend the low 48 bits
        let pageid = ((raw << 16) as i64) >> 16;
        Lsa {
            pageid,
            offset: (raw >> 48) as i16,
        }
    }

    pub fn to_u64(self) -> u64 {
        (self.pageid as u64 & 0xFFFF_FFFF_FFFF) | ((self.offset as u16 as u64) << 48)
    }

    pub fn read(buf: &[u8], off: usize) -> Self {
        Self::from_u64(get_u64(buf, off))
    }

    pub fn write(&self, buf: &mut [u8], off: usize) {
        put_u64(buf, off, self.to_u64());
    }
}

impl std::fmt::Display for Lsa {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}|{}", self.pageid, self.offset)
    }
}

/// Engine `LOG_LSA` as it sits inside a fixed on-disk struct: one little-endian 64-bit word.
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned, Clone, Copy)]
#[repr(C)]
pub struct LsaLe(U64);

impl LsaLe {
    pub fn get(&self) -> Lsa {
        Lsa::from_u64(self.0.get())
    }

    pub fn set(&mut self, l: Lsa) {
        self.0.set(l.to_u64());
    }
}

impl From<Lsa> for LsaLe {
    fn from(l: Lsa) -> Self {
        LsaLe(l.to_u64().into())
    }
}
