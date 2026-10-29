//! The live rows of the database: per heap page, one bit per slot that holds a live row, with the class whose heap
//! holds the page.
//!
//! Input: per heap pass, its pages' live slots ([`LiveSlots`], built page by page during the scan). Output: "is this
//! OID a live row, and of which class" in O(log pages) (index → heap OID survival of deferred indexes, OBJECT values,
//! catalog references), and the OIDs found live in two heaps.
//! Memory (under `--mem`, workspace#509): [`PAGE_BYTES`] per heap page with a live row plus one bit per slot up to its
//! last live slot. The map is reserved before the heap pass at [`ESTIMATE_PAGE_BYTES`] per heap page — so finished
//! runs, which take what is free, cannot crowd it out — and settled to its real size after; a `--mem` that cannot
//! hold it stops the run with the `--mem` that can.
//! Source: G3 workspace#483 Q7 (sorted live OID set), G5 workspace#486 (deferred indexes: OID survival only);
//! workspace#509 (page bitmap instead of 16 B per row: no sort of every live OID, 1/59 of the memory on the big
//! develop DB).

use std::io;

use fastmig_index::{MemoryLimit, memory_limit::MemoryGrant};
use fastmig_ondisk::{Oid, Vpid};

/// The live slots of one heap's pages, as the heap pass finds them.
#[derive(Default)]
pub struct LiveSlots {
    /// (page, first word in `bits`, words).
    pages: Vec<(Vpid, u32, u32)>,
    bits: Vec<u64>,
}

impl LiveSlots {
    /// Records the live rows `oids` of page `vpid` (every OID a slot of that page).
    pub fn push_page(&mut self, vpid: Vpid, oids: &[Oid]) {
        let Some(last) = oids.iter().map(|o| o.slotid.max(0) as usize).max() else {
            return;
        };
        let (at, words) = (self.bits.len(), last / 64 + 1);
        self.bits.resize(at + words, 0);
        for o in oids {
            let s = o.slotid.max(0) as usize;
            self.bits[at + s / 64] |= 1 << (s % 64);
        }
        self.pages.push((vpid, at as u32, words as u32));
    }
}

/// One page of the map.
#[derive(Clone, Copy)]
struct LivePage {
    vpid: Vpid,
    class: Oid,
    at: u32,
    words: u32,
}

/// Bytes of one page entry of the map, besides its slot bits.
pub const PAGE_BYTES: usize = std::mem::size_of::<LivePage>();

/// Reserved per heap page before the heap pass: the entry and five words of slot bits (320 slots; the big develop
/// DB averages 4.25 words, workspace#509). A database of smaller rows takes the rest when it is settled.
pub const ESTIMATE_PAGE_BYTES: usize = PAGE_BYTES + 5 * 8;

pub struct LiveRowMap<'m> {
    memory: &'m MemoryLimit,
    grant: MemoryGrant<'m>,
    pages: Vec<LivePage>,
    bits: Vec<u64>,
    len: usize,
}

const MIB: usize = 1 << 20;

impl<'m> LiveRowMap<'m> {
    /// Reserves the map of `heap_pages` heap pages from `memory`; an error naming the `--mem` that holds it when
    /// the limit cannot.
    pub fn reserve(memory: &'m MemoryLimit, heap_pages: usize) -> io::Result<Self> {
        let need = heap_pages * ESTIMATE_PAGE_BYTES;
        let grant = memory.try_acquire(need);
        let map = LiveRowMap {
            memory,
            grant,
            pages: Vec::with_capacity(heap_pages),
            bits: Vec::new(),
            len: 0,
        };
        map.short_of(need, heap_pages)?;
        Ok(map)
    }

    /// Adds the live rows of one heap of `class`, taking more of `--mem` when the map outgrows its grant.
    pub fn add(&mut self, class: Oid, s: LiveSlots) -> io::Result<()> {
        let base = self.bits.len() as u32;
        // exact growth: the map is charged to --mem by capacity, a doubling would charge up to twice its size
        self.pages.reserve_exact(s.pages.len());
        self.pages
            .extend(s.pages.iter().map(|&(vpid, at, words)| LivePage {
                vpid,
                class,
                at: base + at,
                words,
            }));
        self.bits.reserve_exact(s.bits.len());
        self.bits.extend_from_slice(&s.bits);
        drop(s);
        let need = self.bytes();
        if need > self.grant.bytes {
            let more = self.memory.try_acquire(need - self.grant.bytes);
            self.grant.absorb(more);
            self.short_of(need, self.pages.len())?;
        }
        Ok(())
    }

    fn bytes(&self) -> usize {
        self.pages.capacity() * PAGE_BYTES + self.bits.capacity() * 8
    }

    fn short_of(&self, need: usize, pages: usize) -> io::Result<()> {
        if self.grant.bytes >= need {
            return Ok(());
        }
        let limit = self.memory.limit();
        Err(io::Error::other(format!(
            "live row map of {pages} heap pages needs {} MiB of --mem; --mem {} MiB left {} MiB for it: rerun \
             with --mem {}M or more",
            need.div_ceil(MIB),
            limit / MIB,
            self.grant.bytes / MIB,
            (limit + need - self.grant.bytes).div_ceil(MIB)
        )))
    }

    /// Sorts the pages; call once after the last `add`. Returns the OIDs found live twice (in two heaps or twice
    /// in one), as (OID, class, other class) with the classes in ascending order.
    pub fn seal(&mut self) -> Vec<(Oid, Oid, Oid)> {
        self.pages.sort_unstable_by_key(|p| (p.vpid, p.class));
        let mut twice = Vec::new();
        let mut len = 0;
        let mut i = 0;
        while i < self.pages.len() {
            let v = self.pages[i].vpid;
            let j = i + self.pages[i..].iter().take_while(|p| p.vpid == v).count();
            let words = self.pages[i..j].iter().map(|p| p.words).max().unwrap_or(0);
            for w in 0..words {
                let mut seen = 0u64;
                for k in i..j {
                    let b = self.word(&self.pages[k], w);
                    // a slot also live in an earlier entry of this page: its latest holder and this one
                    let mut both = seen & b;
                    while both != 0 {
                        let bit = both.trailing_zeros();
                        let prev = self.pages[i..k]
                            .iter()
                            .rev()
                            .find(|q| self.word(q, w) >> bit & 1 == 1)
                            .unwrap();
                        let s = (w * 64 + bit) as i16;
                        let oid = Oid::new(v.volid, v.pageid, s);
                        twice.push((oid, prev.class, self.pages[k].class));
                        both &= both - 1;
                    }
                    seen |= b;
                }
                len += seen.count_ones() as usize;
            }
            i = j;
        }
        self.len = len;
        self.pages.shrink_to_fit();
        // the reservation's unused part goes back to the limit
        let used = self.bytes();
        if self.grant.bytes > used {
            drop(self.grant.split(self.grant.bytes - used));
        }
        twice
    }

    fn word(&self, p: &LivePage, w: u32) -> u64 {
        if w < p.words {
            self.bits[(p.at + w) as usize]
        } else {
            0
        }
    }

    pub fn class_of(&self, oid: Oid) -> Option<Oid> {
        let v = Vpid::new(oid.volid, oid.pageid);
        let s = u32::try_from(oid.slotid).ok()?;
        let from = self.pages.partition_point(|p| p.vpid < v);
        self.pages[from..]
            .iter()
            .take_while(|p| p.vpid == v)
            .find(|p| self.word(p, s / 64) >> (s % 64) & 1 == 1)
            .map(|p| p.class)
    }

    /// Distinct live rows.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}
