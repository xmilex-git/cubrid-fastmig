//! Turns a heap's 11.4 header page into develop's: room for the larger header record, the bestspace pages
//! allocated and linked right after the header, and the develop `HEAP_HDR_STATS` written. A develop header (the
//! same-version path) keeps its record size, shard count and bestspace pages; its statistics, candidates and shard
//! entries are written again by the same rules (workspace#501).
//!
//! Input: a [`HeapState`] after the heap's pages were rewritten, the source header record, whether every chain page
//! was read, and whether this is the root class heap. Output: the header page and bestspace pages written; the
//! [`HeaderOutcome`].
//! Source: workspace#482 decision 6 (bestspace pages between the header and the old first page; header HOME
//! records moved out largest first, equal sizes higher slot first) and 7 (statistics measured only on fully read
//! heaps, otherwise the 11.4 estimates; pages read by descending `total_free`, VPID ascending on ties, 8 shards, 1 for
//! the root heap) with workspace#501 (the first 128 into the candidate queue, the rest round-robin over the
//! shards); engine develop @5b1867174 `heap_file.c` `heap_create_bestspace` (:3896-4073),
//! `heap_update_statistics` (:9024-9110), `heap_load_bestspace` (:4410-4480), `bestspace.cpp` `bestspace::find`; R2 § 1.4, § 1.5, § 3 R6.

use std::io;

use crate::heap_rewrite::heap_state::{HeapState, PageStats, chain_of, editor, invalid, spage_err};
use crate::page::{self, PageType, db_area, db_area_mut};
use crate::record::bestspace_page::{self, DEFAULT_SHARDS, ENTRIES_PER_SHARD, ROOT_SHARDS};
use crate::record::heap_hdr_stats::{
    BestspaceDir, BestspaceEntry, HeapHdrStats, LineFields, MAX_CANDIDATES,
};
use crate::spage::{RecordType, SlottedPage};
use crate::{VersionLine, Vpid};

/// What the header rewrite did.
#[derive(Clone, Debug, Default)]
pub struct HeaderOutcome {
    pub bestspace_pages: Vec<Vpid>,
    /// Header-page records moved to another page to make room.
    pub evicted: u64,
    /// The statistics written (measured, or the 11.4 estimates).
    pub stats: PageStats,
}

/// Moves HOME records off the header page, largest first, until its slot 0 can grow to the develop size.
fn make_room(st: &mut HeapState, hv: Vpid) -> io::Result<u64> {
    let need = HeapHdrStats::size(VersionLine::Develop);
    let mut evicted = 0;
    loop {
        let (fits, victim) = st.peek(hv, |p| {
            let mut copy = p.to_vec();
            let ed = editor(&mut copy, hv)?;
            let sp = ed.view();
            let victim = (1..sp.num_slots())
                .filter_map(|s| {
                    sp.record(s)
                        .filter(|(t, _)| *t == RecordType::Home)
                        .map(|(_, r)| (r.len(), s, r.to_vec()))
                })
                .max_by_key(|(len, s, _)| (*len, *s));
            Ok((ed.can_update(0, need), victim))
        })?;
        if fits {
            return Ok(evicted);
        }
        let (_, slot, rec) = victim.ok_or_else(|| {
            invalid(format!(
                "header page {hv:?}: no room for the develop header"
            ))
        })?;
        let nh = st.insert_newhome(&rec, hv)?;
        let mut ptr = [0u8; 8];
        nh.write(&mut ptr, 0);
        st.edit(hv, |p| {
            let mut ed = editor(p, hv)?;
            ed.update(slot, &ptr)
                .and_then(|()| ed.set_record_type(slot, RecordType::Relocation))
                .map_err(|e| spage_err(hv, e))
        })?;
        evicted += 1;
    }
}

/// Rewrites the header page of `st`'s heap. `src` is its 11.4 header record; `measured` says every chain page was
/// read (statistics come from them); `root` selects the root heap's single shard.
pub(crate) fn rewrite_header(
    st: &mut HeapState,
    src: &HeapHdrStats,
    measured: bool,
    root: bool,
    target_line: VersionLine,
) -> io::Result<HeaderOutcome> {
    let hv = st.hfid.vpid();
    let io_size = st.vs().io_pagesize();
    st.load(hv)?;
    if target_line != VersionLine::Develop {
        return write_header_114(st, src, measured);
    }
    let evicted = make_room(st, hv)?;
    st.header_reserve = 0;
    // a develop source has its bestspace pages already: the first data page follows the last of them
    if let LineFields::Develop(d) = &src.line {
        let last = *d
            .pages
            .last()
            .ok_or_else(|| invalid(format!("{hv:?}: develop header without bestspace pages")))?;
        let first = st.peek(last, |p| chain_of(p, last))?.next_vpid;
        return write_header_and_bestspace(
            st,
            src,
            measured,
            d.pages.clone(),
            first,
            d.num_shards,
            false,
            evicted,
        );
    }
    // the link may have moved if a page was appended to a one-page heap
    let old_first = st.peek(hv, |p| {
        let sp = SlottedPage::new(db_area(p))
            .ok_or_else(|| invalid(format!("{hv:?}: not a slotted page")))?;
        Ok(Vpid::read(
            sp.record(0)
                .ok_or_else(|| invalid(format!("{hv:?}: no header record")))?
                .1,
            16,
        ))
    })?;
    let bs = (0..bestspace_page::page_count(io_size))
        .map(|_| st.space.alloc_page(st.hfid.vfid, PageType::Heap))
        .collect::<io::Result<Vec<_>>>()?;
    let num_shards = if root { ROOT_SHARDS } else { DEFAULT_SHARDS };
    write_header_and_bestspace(st, src, measured, bs, old_first, num_shards, true, evicted)
}

/// An 11.4 target: the 11.4 source's 296 B header record stays in place with its best-page hints; the counts, the last
/// page and the overflow file are written again (measured counts when every chain page was read). A hint names a page
/// whose free space only grew (vacuum residue removed), and 11.4 checks a hinted page's free space before using it
/// (workspace#533; R15 § 4.2: no bestspace pages in 11.4).
fn write_header_114(
    st: &mut HeapState,
    src: &HeapHdrStats,
    measured: bool,
) -> io::Result<HeaderOutcome> {
    let hv = st.hfid.vpid();
    if !matches!(src.line, LineFields::V11_4(_)) {
        return Err(invalid(format!(
            "{hv:?}: a develop heap header cannot become 11.4's (downgrade, ADR 0003)"
        )));
    }
    st.flush()?;
    let stats = if measured {
        let mut s = PageStats::default();
        st.pages().iter().for_each(|p| s.add(p.stats));
        s
    } else {
        PageStats {
            pages: src.num_pages.max(0) as u64 + st.appended_pages,
            recs: src.num_recs,
            sumlen: src.recs_sumlen,
        }
    };
    let hdr = HeapHdrStats {
        ovf_vfid: st.ovf_vfid,
        last_vpid: st.last_vpid,
        num_pages: stats.pages.min(i32::MAX as u64) as i32,
        num_recs: stats.recs,
        recs_sumlen: stats.sumlen,
        ..src.clone()
    };
    let mut rec = vec![0u8; HeapHdrStats::size(VersionLine::V11_4)];
    hdr.write(&mut rec, VersionLine::V11_4);
    st.edit_slot0(hv, |r| r.copy_from_slice(&rec))?;
    st.flush()?;
    Ok(HeaderOutcome {
        bestspace_pages: Vec::new(),
        evicted: 0,
        stats,
    })
}

/// Writes the develop header record and bestspace pages `bs` (shard entries and candidates from the pages read);
/// `old_first` is the first data page after them; `new_pages` says `bs` were just allocated (initialized and
/// linked here) rather than kept from a develop source (page header kept, user area written again).
#[allow(clippy::too_many_arguments)]
fn write_header_and_bestspace(
    st: &mut HeapState,
    src: &HeapHdrStats,
    measured: bool,
    bs: Vec<Vpid>,
    old_first: Vpid,
    num_shards: u64,
    new_pages: bool,
    evicted: u64,
) -> io::Result<HeaderOutcome> {
    let hv = st.hfid.vpid();
    let io_size = st.vs().io_pagesize();
    // the header page as it stands after `make_room`: its evicted records count once, at their NEWHOME
    st.flush()?;
    let n = bs.len();
    let stats = if measured {
        let mut s = PageStats::default();
        st.pages().iter().for_each(|p| s.add(p.stats));
        s
    } else {
        PageStats {
            pages: src.num_pages.max(0) as u64 + st.appended_pages,
            recs: src.num_recs,
            sumlen: src.recs_sumlen,
        }
    };
    let mut hdr = HeapHdrStats {
        class_oid: src.class_oid,
        ovf_vfid: st.ovf_vfid,
        next_vpid: bs[0],
        last_vpid: if old_first.is_null() {
            bs[n - 1]
        } else {
            st.last_vpid
        },
        unfill_space: src.unfill_space,
        num_pages: stats.pages.min(i32::MAX as u64) as i32,
        num_recs: stats.recs,
        recs_sumlen: stats.sumlen,
        line: LineFields::Develop(BestspaceDir {
            candidates: Vec::new(),
            num_shards,
            pages: bs.clone(),
        }),
    };
    let mut rec = vec![0u8; HeapHdrStats::size(VersionLine::Develop)];
    hdr.write(&mut rec, VersionLine::Develop);
    st.edit(hv, |p| {
        editor(p, hv)?.update(0, &rec).map_err(|e| spage_err(hv, e))
    })?;
    st.flush()?;

    // entries from every page read, the header's free space as it now stands
    let mut ranked: Vec<(i32, Vpid)> = st.pages().iter().map(|p| (p.total_free, p.vpid)).collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let entry = |&(free, vpid): &(i32, Vpid)| BestspaceEntry {
        freespace: free.clamp(0, u16::MAX as i32) as u16,
        vpid,
    };
    // the candidate queue first: every insert pops it before its own shard, as engine vacuum feeds it
    // (bestspace.cpp bestspace::find, vacuum.c heap_add_bestpage); the rest round-robin so every thread's shard
    // (thread index % shards) sees free pages (workspace#501)
    let candidates: Vec<BestspaceEntry> = ranked.iter().take(MAX_CANDIDATES).map(entry).collect();
    let shards = num_shards as usize;
    let mut shard = vec![BestspaceEntry::NULL; shards * ENTRIES_PER_SHARD];
    for (i, e) in ranked
        .iter()
        .skip(MAX_CANDIDATES)
        .take(shard.len())
        .enumerate()
    {
        shard[(i % shards) * ENTRIES_PER_SHARD + i / shards] = entry(e);
    }
    if let LineFields::Develop(d) = &mut hdr.line {
        d.candidates = candidates;
    }
    hdr.write(&mut rec, VersionLine::Develop);
    st.edit_slot0(hv, |r| r.copy_from_slice(&rec))?;
    st.flush()?;

    let mut from = 0;
    for (i, &vpid) in bs.iter().enumerate() {
        let take =
            bestspace_page::entries_in_page(io_size, i).min(shard.len().saturating_sub(from));
        let mut p = if new_pages {
            let mut p = vec![0u8; io_size];
            page::init_header(&mut p, vpid, PageType::Heap);
            p
        } else {
            // the page header (its LSA) stays; the user area is written again
            st.vs().read_page(vpid)?.to_vec()
        };
        bestspace_page::write(
            db_area_mut(&mut p),
            io_size,
            i,
            src.class_oid,
            if i == 0 { hv } else { bs[i - 1] },
            if i + 1 == n { old_first } else { bs[i + 1] },
            &shard[from..from + take],
        );
        from += take;
        st.vs().write_page(vpid, &p)?;
    }
    if new_pages && !old_first.is_null() {
        st.set_prev(old_first, bs[n - 1])?;
    }
    Ok(HeaderOutcome {
        bestspace_pages: bs,
        evicted,
        stats,
    })
}
