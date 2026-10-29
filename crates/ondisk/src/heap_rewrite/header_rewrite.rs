//! Turns a heap's 11.4 header page into develop's: room for the larger header record, the bestspace pages
//! allocated and linked right after the header, and the develop `HEAP_HDR_STATS` written.
//!
//! Input: a [`HeapState`] after the heap's pages were rewritten, the 11.4 header record, whether every chain page
//! was read, and whether this is the root class heap. Output: the header page and bestspace pages written; the
//! [`HeaderOutcome`].
//! Source: workspace#482 decision 6 (bestspace pages between the header and the old first page; header HOME
//! records moved out largest first, equal sizes higher slot first) and 7 (statistics measured only on fully read
//! heaps, otherwise the 11.4 estimates; shard entries and candidates = pages read by descending `total_free`, VPID
//! ascending on ties, 8 shards, 1 for the root heap); engine develop @5b1867174 `heap_file.c`
//! `heap_create_bestspace` (:3896-4073), `heap_update_statistics` (:9024-9110), `heap_load_bestspace`
//! (:4410-4480); R2 § 1.4, § 1.5, § 3 R6.

use std::io;

use crate::heap_rewrite::heap_state::{HeapState, PageStats, editor, invalid, spage_err};
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
) -> io::Result<HeaderOutcome> {
    let hv = st.hfid.vpid();
    let io_size = st.vs().io_pagesize();
    st.load(hv)?;
    let evicted = make_room(st, hv)?;
    st.header_reserve = 0;
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
    let n = bestspace_page::page_count(io_size);
    let bs = (0..n)
        .map(|_| st.space.alloc_page(st.hfid.vfid, PageType::Heap))
        .collect::<io::Result<Vec<_>>>()?;

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
    let num_shards = if root { ROOT_SHARDS } else { DEFAULT_SHARDS };
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
    let n_shard = num_shards as usize * ENTRIES_PER_SHARD;
    let shard: Vec<BestspaceEntry> = ranked.iter().take(n_shard).map(entry).collect();
    let candidates: Vec<BestspaceEntry> = ranked
        .iter()
        .skip(n_shard)
        .take(MAX_CANDIDATES)
        .map(entry)
        .collect();
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
        let mut p = vec![0u8; io_size];
        page::init_header(&mut p, vpid, PageType::Heap);
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
    if !old_first.is_null() {
        st.set_prev(old_first, bs[n - 1])?;
    }
    Ok(HeaderOutcome {
        bestspace_pages: bs,
        evicted,
        stats,
    })
}
