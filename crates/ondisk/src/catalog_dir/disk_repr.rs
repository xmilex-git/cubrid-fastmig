//! The disk-representation record of one class: its storage layout (fixed then variable attributes), each
//! attribute's default value and the statistics of the indexes on it. The record may run over several catalog
//! pages as fragments.
//!
//! Input: the record's fragments in chain order, and the release line. Output: [`DiskRepr`], or the develop
//! fragments for given fragment capacities.
//! Source: engine `system_catalog.c` `catalog_store_disk_representation` / `_disk_attribute` /
//! `_attribute_value` / `_btree_statistics` and the matching `catalog_fetch_*` (a structure never splits: if it
//! does not fit the rest of a fragment, the fragment ends and the structure starts the next one; a default value
//! does split), `CATALOG_DISK_REPR_*` (56 B), `CATALOG_DISK_ATTR_*` (88 B, ndv INT64 @80), `CATALOG_BT_STATS_*`
//! (11.4: 80 B, keys int @24, pkeys int x 8 @32; develop: 120 B, @24 = 0, keys INT64 @32, pkeys INT64 x 8 @40,
//! 519cc1a43 PR #7856); release/11.4 @46212adcd, develop @5b1867174; R3 § 3, rules E1-E2.

use super::obj_ids::{read_btid, read_oid, write_btid, write_oid};
use crate::value::{DecodeError, DecodeResult, OrReader, OrWriter};
use crate::{Btid, Oid, VersionLine};

/// Engine `BTREE_STATS_PKEYS_NUM`.
pub const PKEYS_NUM: usize = 8;
const DISK_REPR_SIZE: usize = 56;
const DISK_ATTR_SIZE: usize = 88;

fn bt_stats_size(line: VersionLine) -> usize {
    match line {
        VersionLine::V11_4 => 80,
        VersionLine::Develop => 120,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiskRepr {
    pub id: i32,
    pub fixed_length: i32,
    pub fixed: Vec<DiskAttr>,
    pub variable: Vec<DiskAttr>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiskAttr {
    pub id: i32,
    /// Byte offset in the fixed area, or the index among variable attributes.
    pub location: i32,
    pub type_id: i32,
    pub position: i32,
    pub class_oid: Oid,
    pub ndv: i64,
    /// Default value bytes as stored (`val_length` = its length).
    pub value: Vec<u8>,
    pub bt_stats: Vec<BtStats>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BtStats {
    pub btid: Btid,
    pub leafs: i32,
    pub pages: i32,
    pub height: i32,
    pub has_function: i32,
    pub keys: i64,
    pub pkeys: [i64; PKEYS_NUM],
}

impl BtStats {
    /// Statistics of a freshly built index whose counts `UPDATE STATISTICS` fills later (rule E2).
    pub fn empty(btid: Btid, has_function: bool) -> Self {
        BtStats {
            btid,
            leafs: 0,
            pages: 0,
            height: 0,
            has_function: has_function as i32,
            keys: 0,
            pkeys: [0; PKEYS_NUM],
        }
    }
}

/// Reads structures across fragments the way `catalog_read_unread_portion` does.
struct Fragments<'a> {
    frags: &'a [&'a [u8]],
    idx: usize,
    pos: usize,
}

impl<'a> Fragments<'a> {
    fn err(&self, what: String) -> DecodeError {
        DecodeError {
            at: self.pos,
            what: format!("fragment {}: {what}", self.idx),
        }
    }

    /// The next `n` bytes of one structure, moving to the next fragment when this one has fewer left.
    fn structure(&mut self, n: usize) -> DecodeResult<OrReader<'a>> {
        if self.frags[self.idx].len() - self.pos < n {
            self.idx += 1;
            self.pos = 0;
            if self.idx >= self.frags.len() || self.frags[self.idx].len() < n {
                return Err(self.err(format!("no fragment holds the next {n} B structure")));
            }
        }
        let s = &self.frags[self.idx][self.pos..self.pos + n];
        self.pos += n;
        Ok(OrReader::new(s))
    }

    /// A default value, which may continue in following fragments.
    fn value(&mut self, mut n: usize) -> DecodeResult<Vec<u8>> {
        let mut out = Vec::with_capacity(n);
        while n > 0 {
            let left = self.frags[self.idx].len() - self.pos;
            if left == 0 {
                self.idx += 1;
                self.pos = 0;
                if self.idx >= self.frags.len() {
                    return Err(self.err(format!("value runs past the last fragment by {n} B")));
                }
                continue;
            }
            let take = left.min(n);
            out.extend_from_slice(&self.frags[self.idx][self.pos..self.pos + take]);
            self.pos += take;
            n -= take;
        }
        Ok(out)
    }
}

impl DiskRepr {
    pub fn decode(frags: &[&[u8]], line: VersionLine) -> DecodeResult<Self> {
        if frags.is_empty() {
            return Err(DecodeError {
                at: 0,
                what: "no fragments".into(),
            });
        }
        let mut f = Fragments {
            frags,
            idx: 0,
            pos: 0,
        };
        let mut r = f.structure(DISK_REPR_SIZE)?;
        let id = r.i32()?;
        let n_fixed = r.i32()?;
        let fixed_length = r.i32()?;
        let n_variable = r.i32()?;
        let mut attrs = Vec::new();
        for _ in 0..n_fixed.max(0) + n_variable.max(0) {
            let mut r = f.structure(DISK_ATTR_SIZE)?;
            let id = r.i32()?;
            let location = r.i32()?;
            let type_id = r.i32()?;
            let val_length = r.i32()?;
            let position = r.i32()?;
            let class_oid = read_oid(&mut r)?;
            let n_btstats = r.i32()?;
            r.skip(80 - r.pos())?;
            let ndv = r.i64()?;
            let value = f.value(val_length.max(0) as usize)?;
            let mut bt_stats = Vec::new();
            for _ in 0..n_btstats.max(0) {
                bt_stats.push(read_bt_stats(f.structure(bt_stats_size(line))?, line)?);
            }
            attrs.push(DiskAttr {
                id,
                location,
                type_id,
                position,
                class_oid,
                ndv,
                value,
                bt_stats,
            });
        }
        let variable = attrs.split_off(n_fixed.max(0) as usize);
        Ok(DiskRepr {
            id,
            fixed_length,
            fixed: attrs,
            variable,
        })
    }

    /// The develop record cut into fragments: the first holds at most `first_cap` bytes, every later one at most
    /// `next_cap` (each `spage_max_space_for_new_record - CATALOG_MAX_SLOT_ID_SIZE` of its page).
    pub fn encode_develop(&self, first_cap: usize, next_cap: usize) -> Vec<Vec<u8>> {
        let mut out = Frags {
            done: Vec::new(),
            cur: OrWriter::new(),
            cap: first_cap,
            next_cap,
        };
        let mut w = OrWriter::new();
        w.i32(self.id);
        w.i32(self.fixed.len() as i32);
        w.i32(self.fixed_length);
        w.i32(self.variable.len() as i32);
        w.zeros(DISK_REPR_SIZE - w.len());
        out.structure(&w.buf);
        for a in self.fixed.iter().chain(&self.variable) {
            let mut w = OrWriter::new();
            w.i32(a.id);
            w.i32(a.location);
            w.i32(a.type_id);
            w.i32(a.value.len() as i32);
            w.i32(a.position);
            write_oid(&mut w, a.class_oid);
            w.i32(a.bt_stats.len() as i32);
            w.zeros(80 - w.len());
            w.i64(a.ndv);
            out.structure(&w.buf);
            out.value(&a.value);
            for s in &a.bt_stats {
                out.structure(&write_bt_stats_develop(s));
            }
        }
        out.done.push(out.cur.buf);
        out.done
    }
}

/// Fragment builder mirroring `catalog_write_unwritten_portion` / `catalog_store_attribute_value`.
struct Frags {
    done: Vec<Vec<u8>>,
    cur: OrWriter,
    cap: usize,
    next_cap: usize,
}

impl Frags {
    fn next(&mut self) {
        self.done.push(std::mem::take(&mut self.cur.buf));
        self.cap = self.next_cap;
    }

    fn structure(&mut self, b: &[u8]) {
        if self.cap - self.cur.len() < b.len() {
            self.next();
        }
        self.cur.bytes(b);
    }

    fn value(&mut self, mut b: &[u8]) {
        if self.cap - self.cur.len() < b.len() {
            self.next();
        }
        while !b.is_empty() {
            let take = (self.cap - self.cur.len()).min(b.len());
            self.cur.bytes(&b[..take]);
            b = &b[take..];
            if !b.is_empty() {
                self.next();
            }
        }
    }
}

fn read_bt_stats(mut r: OrReader, line: VersionLine) -> DecodeResult<BtStats> {
    let btid = read_btid(&mut r)?;
    let leafs = r.i32()?;
    let pages = r.i32()?;
    let height = r.i32()?;
    let keys32 = r.i32()?;
    let has_function = r.i32()?;
    let mut pkeys = [0i64; PKEYS_NUM];
    let keys = match line {
        VersionLine::V11_4 => {
            for p in &mut pkeys {
                *p = r.i32()? as i64;
            }
            keys32 as i64
        }
        VersionLine::Develop => {
            let k = r.i64()?;
            for p in &mut pkeys {
                *p = r.i64()?;
            }
            k
        }
    };
    Ok(BtStats {
        btid,
        leafs,
        pages,
        height,
        has_function,
        keys,
        pkeys,
    })
}

fn write_bt_stats_develop(s: &BtStats) -> Vec<u8> {
    let mut w = OrWriter::new();
    write_btid(&mut w, s.btid);
    w.i32(s.leafs);
    w.i32(s.pages);
    w.i32(s.height);
    w.i32(0); // the 32-bit keys slot is no longer read
    w.i32(s.has_function);
    w.i64(s.keys);
    for p in s.pkeys {
        w.i64(p);
    }
    w.zeros(bt_stats_size(VersionLine::Develop) - w.len());
    w.buf
}
