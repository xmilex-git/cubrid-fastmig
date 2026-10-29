//! The heap file header record (`HEAP_HDR_STATS`, slot 0 of a heap's header page) in its 11.4 and develop layouts.
//!
//! Input: the slot-0 record of a heap header page and the release line that wrote it. Output: [`HeapHdrStats`] —
//! the fields both lines share (class, overflow file, chain links, unfill space, the page/record estimates) plus the
//! line's own part (11.4 best-page hints, develop bestspace directory); or the record written in either layout.
//! Source: engine `src/storage/heap_file.c` `struct heap_hdr_stats` (release/11.4 @46212adcd :192-232, develop
//! @5b1867174 :190-224), `HEAP_BESTSPACE` (release/11.4 `heap_file.h:120-124`), `cubstorage::bestspace_entry`
//! (develop `src/storage/bestspace.hpp`, static_assert 8 B); R2 § 1.4 (offsets computed, checked on T1 header pages
//! in workspace#499). Native (little-endian) layout.

use zerocopy::little_endian::{F32, I16, I32, U16, U64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use crate::ids::{OidLe, VfidLe, VpidLe};
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

/// [`BestspaceEntry`] as stored (its VPID halves are in the opposite order of an engine `VPID`).
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned, Clone, Copy)]
#[repr(C)]
pub struct BestspaceEntryLe {
    freespace: U16,
    volid: I16,
    pageid: I32,
}

impl From<&BestspaceEntryLe> for BestspaceEntry {
    fn from(e: &BestspaceEntryLe) -> Self {
        BestspaceEntry {
            freespace: e.freespace.get(),
            vpid: Vpid::new(e.volid.get(), e.pageid.get()),
        }
    }
}

impl From<&BestspaceEntry> for BestspaceEntryLe {
    fn from(e: &BestspaceEntry) -> Self {
        BestspaceEntryLe {
            freespace: e.freespace.into(),
            volid: e.vpid.volid.into(),
            pageid: e.vpid.pageid.into(),
        }
    }
}

impl BestspaceEntry {
    pub const SIZE: usize = size_of::<BestspaceEntryLe>();
    /// `bestspace_entry::set_null`: no free space, NULL VPID.
    pub const NULL: BestspaceEntry = BestspaceEntry {
        freespace: 0,
        vpid: Vpid::NULL,
    };

    pub fn write(&self, buf: &mut [u8], off: usize) {
        BestspaceEntryLe::from(self)
            .write_to_prefix(&mut buf[off..])
            .expect("entry inside the record");
    }
}

/// 11.4 `HEAP_BESTSPACE`: a page and its free space (12 B).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BestPage {
    pub vpid: Vpid,
    pub freespace: i32,
}

#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct BestPageLe {
    vpid: VpidLe,
    freespace: I32,
}

/// The first 24 B of `HEAP_HDR_STATS`, the same in 11.4 and develop.
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
pub struct HeapHdrStatsHeadLe {
    /// @0
    pub class_oid: OidLe,
    /// @8
    pub ovf_vfid: VfidLe,
    /// @16, the header page's link to the first data page (develop: the first bestspace page).
    pub next_vpid: VpidLe,
}

impl HeapHdrStatsHeadLe {
    /// The shared head of a `HEAP_HDR_STATS` record of either line.
    pub fn of(rec: &[u8]) -> &Self {
        Self::ref_from_prefix(rec)
            .expect("record holds HEAP_HDR_STATS")
            .0
    }

    pub fn of_mut(rec: &mut [u8]) -> &mut Self {
        Self::mut_from_prefix(rec)
            .expect("record holds HEAP_HDR_STATS")
            .0
    }
}

/// 11.4 `HEAP_HDR_STATS` as stored (release/11.4 @46212adcd heap_file.c:192-232), 296 B.
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct HeapHdrStats114Le {
    /// @0
    common: HeapHdrStatsHeadLe,
    /// @24
    unfill_space: I32,
    /// @28, `estimates.num_pages`.
    num_pages: I32,
    /// @32
    num_recs: I32,
    /// @36
    recs_sumlen: F32,
    /// @40
    num_other_high_best: I32,
    /// @44
    num_high_best: I32,
    /// @48
    num_substitutions: I32,
    /// @52
    num_second_best: I32,
    /// @56
    head_second_best: I32,
    /// @60
    tail_second_best: I32,
    /// @64
    head: I32,
    /// @68
    last_vpid: VpidLe,
    /// @76
    full_search_vpid: VpidLe,
    /// @84
    second_best: [VpidLe; NUM_BEST_11_4],
    /// @164
    best: [BestPageLe; NUM_BEST_11_4],
    /// @284, `reserve0..2`.
    reserve: [I32; 3],
}

/// develop `HEAP_HDR_STATS` as stored (develop @5b1867174 heap_file.c:190-224), 1152 B.
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct HeapHdrStatsDevelopLe {
    /// @0
    common: HeapHdrStatsHeadLe,
    /// @24
    last_vpid: VpidLe,
    /// @32
    unfill_space: I32,
    /// @36
    num_pages: I32,
    /// @40
    num_recs: U64,
    /// @48
    recs_sumlen: U64,
    /// @56, `bestspace.num_candidates`.
    num_candidates: U64,
    /// @64
    candidates: [BestspaceEntryLe; MAX_CANDIDATES],
    /// @1088
    num_shards: U64,
    /// @1096, `bestspace.num_pages`.
    num_bestspace_pages: U64,
    /// @1104
    pages: [VpidLe; MAX_SHARD_PAGES],
    /// @1136, `reserve0..2`, then the u64 alignment pad.
    reserve: [I32; 3],
    pad: [u8; 4],
}

const _: () = assert!(size_of::<HeapHdrStats114Le>() == HeapHdrStats::size(VersionLine::V11_4));
const _: () =
    assert!(size_of::<HeapHdrStatsDevelopLe>() == HeapHdrStats::size(VersionLine::Develop));

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
    pub best: [BestPage; NUM_BEST_11_4],
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
            VersionLine::V10_2 | VersionLine::V11_4 => 296,
            VersionLine::Develop => 1152,
        }
    }

    /// Decodes the slot-0 record of a heap header page written by `line`; refuses a record of another length.
    pub fn read(rec: &[u8], line: VersionLine) -> DecodeResult<HeapHdrStats> {
        if rec.len() != Self::size(line) {
            return Err(short(rec, line));
        }
        Ok(match line {
            VersionLine::V10_2 | VersionLine::V11_4 => {
                let h = HeapHdrStats114Le::ref_from_bytes(rec).map_err(|_| short(rec, line))?;
                HeapHdrStats {
                    class_oid: h.common.class_oid.get(),
                    ovf_vfid: h.common.ovf_vfid.get(),
                    next_vpid: h.common.next_vpid.get(),
                    last_vpid: h.last_vpid.get(),
                    unfill_space: h.unfill_space.get(),
                    num_pages: h.num_pages.get(),
                    num_recs: h.num_recs.get().max(0) as u64,
                    recs_sumlen: h.recs_sumlen.get().max(0.0) as u64,
                    line: LineFields::V11_4(Estimates114 {
                        num_other_high_best: h.num_other_high_best.get(),
                        num_high_best: h.num_high_best.get(),
                        num_substitutions: h.num_substitutions.get(),
                        num_second_best: h.num_second_best.get(),
                        head_second_best: h.head_second_best.get(),
                        tail_second_best: h.tail_second_best.get(),
                        head: h.head.get(),
                        full_search_vpid: h.full_search_vpid.get(),
                        second_best: h.second_best.map(|v| v.get()),
                        best: std::array::from_fn(|i| BestPage {
                            vpid: h.best[i].vpid.get(),
                            freespace: h.best[i].freespace.get(),
                        }),
                    }),
                }
            }
            VersionLine::Develop => {
                let h = HeapHdrStatsDevelopLe::ref_from_bytes(rec).map_err(|_| short(rec, line))?;
                let ncand = (h.num_candidates.get() as usize).min(MAX_CANDIDATES);
                let npages = (h.num_bestspace_pages.get() as usize).min(MAX_SHARD_PAGES);
                HeapHdrStats {
                    class_oid: h.common.class_oid.get(),
                    ovf_vfid: h.common.ovf_vfid.get(),
                    next_vpid: h.common.next_vpid.get(),
                    last_vpid: h.last_vpid.get(),
                    unfill_space: h.unfill_space.get(),
                    num_pages: h.num_pages.get(),
                    num_recs: h.num_recs.get(),
                    recs_sumlen: h.recs_sumlen.get(),
                    line: LineFields::Develop(BestspaceDir {
                        candidates: h.candidates[..ncand].iter().map(Into::into).collect(),
                        num_shards: h.num_shards.get(),
                        pages: h.pages[..npages].iter().map(VpidLe::get).collect(),
                    }),
                }
            }
        })
    }

    /// Writes the record in `line`'s layout into `rec` ([`Self::size`] bytes); unused array slots get NULL entries,
    /// reserved words and padding zero. Panics when `line` does not match [`Self::line`]'s variant.
    pub fn write(&self, rec: &mut [u8], line: VersionLine) {
        assert_eq!(rec.len(), Self::size(line), "HEAP_HDR_STATS buffer size");
        rec.fill(0);
        match (&self.line, line) {
            (LineFields::V11_4(e), VersionLine::V10_2 | VersionLine::V11_4) => {
                let h = HeapHdrStats114Le::mut_from_bytes(rec).expect("size checked above");
                h.common.class_oid.set(self.class_oid);
                h.common.ovf_vfid.set(self.ovf_vfid);
                h.common.next_vpid.set(self.next_vpid);
                h.unfill_space.set(self.unfill_space);
                h.num_pages.set(self.num_pages);
                h.num_recs.set(self.num_recs.min(i32::MAX as u64) as i32);
                h.recs_sumlen.set(self.recs_sumlen as f32);
                h.num_other_high_best.set(e.num_other_high_best);
                h.num_high_best.set(e.num_high_best);
                h.num_substitutions.set(e.num_substitutions);
                h.num_second_best.set(e.num_second_best);
                h.head_second_best.set(e.head_second_best);
                h.tail_second_best.set(e.tail_second_best);
                h.head.set(e.head);
                h.last_vpid.set(self.last_vpid);
                h.full_search_vpid.set(e.full_search_vpid);
                h.second_best = e.second_best.map(VpidLe::from);
                h.best = e.best.map(|b| BestPageLe {
                    vpid: b.vpid.into(),
                    freespace: b.freespace.into(),
                });
            }
            (LineFields::Develop(d), VersionLine::Develop) => {
                assert!(d.candidates.len() <= MAX_CANDIDATES && d.pages.len() <= MAX_SHARD_PAGES);
                let h = HeapHdrStatsDevelopLe::mut_from_bytes(rec).expect("size checked above");
                h.common.class_oid.set(self.class_oid);
                h.common.ovf_vfid.set(self.ovf_vfid);
                h.common.next_vpid.set(self.next_vpid);
                h.last_vpid.set(self.last_vpid);
                h.unfill_space.set(self.unfill_space);
                h.num_pages.set(self.num_pages);
                h.num_recs.set(self.num_recs);
                h.recs_sumlen.set(self.recs_sumlen);
                h.num_candidates.set(d.candidates.len() as u64);
                for (i, slot) in h.candidates.iter_mut().enumerate() {
                    *slot = d.candidates.get(i).unwrap_or(&BestspaceEntry::NULL).into();
                }
                h.num_shards.set(d.num_shards);
                h.num_bestspace_pages.set(d.pages.len() as u64);
                for (i, slot) in h.pages.iter_mut().enumerate() {
                    slot.set(*d.pages.get(i).unwrap_or(&Vpid::NULL));
                }
            }
            (_, line) => panic!("HeapHdrStats of another line written as {line:?}"),
        }
    }
}
