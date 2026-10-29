//! Overflow OID pages of a leaf record and the OID directory over them.
//!
//! Input/output: slot 0 (header) and slot 1 (objects, or directory entries) of an overflow OID page.
//! Source: engine `BTREE_OVERFLOW_HEADER`, `BTREE_OVF_DIR_HEADER`, `BTREE_OVF_DIR_ENTRY` (btree_load.h),
//! `bt_load_nospace_for_new_oid`, `bt_load_finalize_ovf_dir`, `btree_ovf_dir_get_header` (btree_load.c),
//! develop 5b1867174; R6 § 3.5. 11.4 has only the 8 B header (directory added by CBRD-24094, #7447).
//!
//! develop: a unique index keeps the 8 B header; a non-unique index writes the 16 B header on every data page,
//! `dir_vpid` NULL while the chain has one data page. With two or more, `dir_vpid` on every data page names the
//! first directory page, whose slot 1 lists one entry per data page in chain order (`sep_oid` = the page's
//! first OID), chained by `next_vpid` every `dir_max_entries`.

use crate::ids::{Oid, Vpid};

/// Header record of an overflow OID page (host order). The format is told by the record length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OvfHeader {
    /// @0 next page of the chain (next data page, or next directory page on a directory page).
    pub next: Vpid,
    /// @8 first directory page, only in the 16 B form; `None` for the 8 B legacy form.
    pub dir: Option<Vpid>,
}

impl OvfHeader {
    pub const LEGACY_SIZE: usize = 8;
    pub const DIR_SIZE: usize = 16;

    pub fn read(b: &[u8]) -> Result<Self, &'static str> {
        match b.len() {
            n if n < Self::LEGACY_SIZE => Err("overflow header shorter than a VPID"),
            n if n < Self::DIR_SIZE => Ok(Self {
                next: Vpid::read(b, 0),
                dir: None,
            }),
            _ => Ok(Self {
                next: Vpid::read(b, 0),
                dir: Some(Vpid::read(b, 8)),
            }),
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut b = vec![
            0u8;
            if self.dir.is_some() {
                Self::DIR_SIZE
            } else {
                Self::LEGACY_SIZE
            }
        ];
        self.next.write(&mut b, 0);
        if let Some(d) = self.dir {
            d.write(&mut b, 8);
        }
        b
    }
}

/// Engine `BTREE_OVF_DIR_ENTRY` (host order). Size 16 B: `sep_oid` OID @0, `vpid` @8.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OvfDirEntry {
    pub sep_oid: Oid,
    pub vpid: Vpid,
}

impl OvfDirEntry {
    pub const SIZE: usize = 16;

    pub fn read_all(b: &[u8]) -> Vec<Self> {
        b.as_chunks::<{ Self::SIZE }>()
            .0
            .iter()
            .map(|c| Self {
                sep_oid: Oid::read(c, 0),
                vpid: Vpid::read(c, 8),
            })
            .collect()
    }

    pub fn write(&self, out: &mut Vec<u8>) {
        let mut b = [0u8; Self::SIZE];
        self.sep_oid.write(&mut b, 0);
        self.vpid.write(&mut b, 8);
        out.extend_from_slice(&b);
    }
}

/// Engine `BTREE_OVF_DIR_MAX_ENTRIES` = `(DB_PAGESIZE - 256) / 16`: 1005 for a 16 KiB IO page.
pub const fn dir_max_entries(db_pagesize: usize) -> usize {
    (db_pagesize - 256) / OvfDirEntry::SIZE
}
