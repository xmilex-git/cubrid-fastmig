//! The heap file header record (`HEAP_HDR_STATS`, slot 0 of a heap's header page) in its 11.4 and develop layouts.
//!
//! Input: the slot-0 record of a heap header page and the release line that wrote it. Output: [`HeapHdrStats`] —
//! the fields both lines share (class, overflow file, chain links, unfill space, the page/record estimates) plus the
//! line's own part (11.4 best-page hints, develop bestspace directory); or the record written in either layout.
//! Source: engine `src/storage/heap_file.c` `struct heap_hdr_stats` (release/11.4 @46212adcd :192-232, develop
//! @5b1867174 :190-224), `HEAP_BESTSPACE` (release/11.4 `heap_file.h:120-124`), `cubstorage::bestspace_entry`
//! (develop `src/storage/bestspace.hpp`, static_assert 8 B); R2 § 1.4 (offsets computed, checked on T1 header pages
//! in workspace#499). Native (little-endian) layout.

use crate::le_field::{get_f32, get_i32, get_u16, get_u64, put_f32, put_i32, put_u16, put_u64};
use crate::value::{DecodeError, DecodeResult};
use crate::{Oid, VersionLine, Vfid, Vpid};

/// 11.4 `HEAP_NUM_BEST_SPACESTATS`.
pub const NUM_BEST_11_4: usize = 10;
/// develop `bestspace::MAX_CANDIDATES_QUEUE_SIZE`.
pub const MAX_CANDIDATES: usize = 128;
/// develop `bestspace::MAX_SHARD_PAGE_COUNT`.
pub const MAX_SHARD_PAGES: usize = 4;

/// develop `cubstorage::bestspace_entry`, 8 B: `freespace` u16 @0, `volid` i16 @2, `pageid` i32 @4.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BestspaceEntry {
    pub freespace: u16,
    pub vpid: Vpid,
}

impl BestspaceEntry {
    pub const SIZE: usize = 8;
    /// `bestspace_entry::set_null`: no free space, NULL VPID.
    pub const NULL: BestspaceEntry = BestspaceEntry {
        freespace: 0,
        vpid: Vpid::NULL,
    };

    pub fn read(buf: &[u8], off: usize) -> Self {
        BestspaceEntry {
            freespace: get_u16(buf, off),
            vpid: Vpid::new(
                i16::from_le_bytes([buf[off + 2], buf[off + 3]]),
                get_i32(buf, off + 4),
            ),
        }
    }

    pub fn write(&self, buf: &mut [u8], off: usize) {
        put_u16(buf, off, self.freespace);
        buf[off + 2..off + 4].copy_from_slice(&self.vpid.volid.to_le_bytes());
        put_i32(buf, off + 4, self.vpid.pageid);
    }
}

/// 11.4 `estimates` fields other than the counts every line has: the best-page hint arrays.
/// Offsets (11.4): `num_other_high_best` @40, `num_high_best` @44, `num_substitutions` @48, `num_second_best` @52,
/// `head_second_best` @56, `tail_second_best` @60, `head` @64, `full_search_vpid` @76, `second_best[10]` @84
/// (VPID 8 B each), `best[10]` @164 (`HEAP_BESTSPACE`: VPID @0, `freespace` i32 @8, 12 B each).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Estimates114 {
    pub num_other_high_best: i32,
    pub num_high_best: i32,
    pub num_substitutions: i32,
    pub num_second_best: i32,
    pub head_second_best: i32,
    pub tail_second_best: i32,
    pub head: i32,
    pub full_search_vpid: Vpid,
    pub second_best: [Vpid; NUM_BEST_11_4],
    pub best: [(Vpid, i32); NUM_BEST_11_4],
}

/// develop `heap_hdr_stats.bestspace`. Offsets (develop): `num_candidates` u64 @56, `candidates[128]` @64,
/// `num_shards` u64 @1088, `num_pages` u64 @1096, `pages[4]` @1104.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BestspaceDir {
    /// The first `candidates.len()` entries of the candidate queue (`num_candidates`, at most [`MAX_CANDIDATES`]).
    pub candidates: Vec<BestspaceEntry>,
    pub num_shards: u64,
    /// The bestspace pages, in chain order (`num_pages` of them, at most [`MAX_SHARD_PAGES`]).
    pub pages: Vec<Vpid>,
}

/// The part of the header only one line has.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LineFields {
    V11_4(Estimates114),
    Develop(BestspaceDir),
}

/// Engine `HEAP_HDR_STATS`. Size: 296 B in 11.4, 1152 B in develop (u64 alignment pads 1148 to 1152).
/// Offsets of the shared fields:
///
/// | field | 11.4 | develop |
/// |---|---|---|
/// | `class_oid` | 0 | 0 |
/// | `ovf_vfid` | 8 | 8 |
/// | `next_vpid` | 16 | 16 |
/// | `last_vpid` | 68 (`estimates.last_vpid`) | 24 |
/// | `unfill_space` i32 | 24 | 32 |
/// | `num_pages` i32 | 28 | 36 |
/// | `num_recs` | 32 (i32) | 40 (u64) |
/// | `recs_sumlen` | 36 (f32) | 48 (u64) |
/// | reserve0..2 | 284 | 1136 |
#[derive(Clone, Debug, PartialEq)]
pub struct HeapHdrStats {
    pub class_oid: Oid,
    /// The heap's overflow file (`MultipageObjectHeap`), NULL when it has none yet.
    pub ovf_vfid: Vfid,
    /// First page after the header page (develop: the first bestspace page).
    pub next_vpid: Vpid,
    /// Last page of the chain.
    pub last_vpid: Vpid,
    pub unfill_space: i32,
    /// Estimated user heap pages (develop: bestspace pages excluded).
    pub num_pages: i32,
    pub num_recs: u64,
    /// Estimated total record length (11.4 stores a float; read truncated, written as the nearest float).
    pub recs_sumlen: u64,
    pub line: LineFields,
}

fn short(rec: &[u8], line: VersionLine) -> DecodeError {
    DecodeError {
        at: 0,
        what: format!(
            "heap header record of {} B, {line:?} HEAP_HDR_STATS is {} B",
            rec.len(),
            HeapHdrStats::size(line)
        ),
    }
}

impl HeapHdrStats {
    /// Record length of `HEAP_HDR_STATS` in `line`.
    pub const fn size(line: VersionLine) -> usize {
        match line {
            VersionLine::V11_4 => 296,
            VersionLine::Develop => 1152,
        }
    }

    /// Decodes the slot-0 record of a heap header page written by `line`; refuses a record of another length.
    pub fn read(rec: &[u8], line: VersionLine) -> DecodeResult<HeapHdrStats> {
        if rec.len() != Self::size(line) {
            return Err(short(rec, line));
        }
        let shared =
            |last_vpid, unfill_space, num_pages, num_recs, recs_sumlen, line| HeapHdrStats {
                class_oid: Oid::read(rec, 0),
                ovf_vfid: Vfid::read(rec, 8),
                next_vpid: Vpid::read(rec, 16),
                last_vpid,
                unfill_space,
                num_pages,
                num_recs,
                recs_sumlen,
                line,
            };
        Ok(match line {
            VersionLine::V11_4 => {
                let est = Estimates114 {
                    num_other_high_best: get_i32(rec, 40),
                    num_high_best: get_i32(rec, 44),
                    num_substitutions: get_i32(rec, 48),
                    num_second_best: get_i32(rec, 52),
                    head_second_best: get_i32(rec, 56),
                    tail_second_best: get_i32(rec, 60),
                    head: get_i32(rec, 64),
                    full_search_vpid: Vpid::read(rec, 76),
                    second_best: std::array::from_fn(|i| Vpid::read(rec, 84 + 8 * i)),
                    best: std::array::from_fn(|i| {
                        (Vpid::read(rec, 164 + 12 * i), get_i32(rec, 172 + 12 * i))
                    }),
                };
                shared(
                    Vpid::read(rec, 68),
                    get_i32(rec, 24),
                    get_i32(rec, 28),
                    get_i32(rec, 32).max(0) as u64,
                    get_f32(rec, 36).max(0.0) as u64,
                    LineFields::V11_4(est),
                )
            }
            VersionLine::Develop => {
                let ncand = (get_u64(rec, 56) as usize).min(MAX_CANDIDATES);
                let npages = (get_u64(rec, 1096) as usize).min(MAX_SHARD_PAGES);
                let dir = BestspaceDir {
                    candidates: (0..ncand)
                        .map(|i| BestspaceEntry::read(rec, 64 + 8 * i))
                        .collect(),
                    num_shards: get_u64(rec, 1088),
                    pages: (0..npages).map(|i| Vpid::read(rec, 1104 + 8 * i)).collect(),
                };
                shared(
                    Vpid::read(rec, 24),
                    get_i32(rec, 32),
                    get_i32(rec, 36),
                    get_u64(rec, 40),
                    get_u64(rec, 48),
                    LineFields::Develop(dir),
                )
            }
        })
    }

    /// Writes the record in `line`'s layout into `rec` ([`Self::size`] bytes); unused array slots get NULL entries,
    /// reserved words and padding zero. Panics when `line` does not match [`Self::line`]'s variant.
    pub fn write(&self, rec: &mut [u8], line: VersionLine) {
        assert_eq!(rec.len(), Self::size(line), "HEAP_HDR_STATS buffer size");
        rec.fill(0);
        self.class_oid.write(rec, 0);
        self.ovf_vfid.write(rec, 8);
        self.next_vpid.write(rec, 16);
        match (&self.line, line) {
            (LineFields::V11_4(e), VersionLine::V11_4) => {
                put_i32(rec, 24, self.unfill_space);
                put_i32(rec, 28, self.num_pages);
                put_i32(rec, 32, self.num_recs.min(i32::MAX as u64) as i32);
                put_f32(rec, 36, self.recs_sumlen as f32);
                for (off, v) in [
                    (40, e.num_other_high_best),
                    (44, e.num_high_best),
                    (48, e.num_substitutions),
                    (52, e.num_second_best),
                    (56, e.head_second_best),
                    (60, e.tail_second_best),
                    (64, e.head),
                ] {
                    put_i32(rec, off, v);
                }
                self.last_vpid.write(rec, 68);
                e.full_search_vpid.write(rec, 76);
                for (i, v) in e.second_best.iter().enumerate() {
                    v.write(rec, 84 + 8 * i);
                }
                for (i, (v, free)) in e.best.iter().enumerate() {
                    v.write(rec, 164 + 12 * i);
                    put_i32(rec, 172 + 12 * i, *free);
                }
            }
            (LineFields::Develop(d), VersionLine::Develop) => {
                assert!(d.candidates.len() <= MAX_CANDIDATES && d.pages.len() <= MAX_SHARD_PAGES);
                self.last_vpid.write(rec, 24);
                put_i32(rec, 32, self.unfill_space);
                put_i32(rec, 36, self.num_pages);
                put_u64(rec, 40, self.num_recs);
                put_u64(rec, 48, self.recs_sumlen);
                put_u64(rec, 56, d.candidates.len() as u64);
                for i in 0..MAX_CANDIDATES {
                    d.candidates
                        .get(i)
                        .unwrap_or(&BestspaceEntry::NULL)
                        .write(rec, 64 + 8 * i);
                }
                put_u64(rec, 1088, d.num_shards);
                put_u64(rec, 1096, d.pages.len() as u64);
                for i in 0..MAX_SHARD_PAGES {
                    d.pages
                        .get(i)
                        .unwrap_or(&Vpid::NULL)
                        .write(rec, 1104 + 8 * i);
                }
            }
            (_, line) => panic!("HeapHdrStats of another line written as {line:?}"),
        }
    }
}
