//! Check 1 (i) and (iii) over one heap pass: the page chain from the header page visits exactly the heap file's user
//! pages, once each, with symmetric prev/next links; every REC_RELOCATION names a REC_NEWHOME of this heap and every
//! REC_NEWHOME is named exactly once; bestspace entries name record pages of the heap (develop; the header page counts); and, for a class whose
//! header statistics the migration measured, `num_recs`/`recs_sumlen` equal the pass's counts.
//!
//! Input: a [`HeapPass`] and whether the class's statistics are measured. Output: check 1 defects.
//! Source: G3 workspace#483 Q5 (i), (iii); G2 workspace#482 (measured classes only; bestspace VPIDs only);
//! engine `heap_file.c` `heap_chain` links, `HEAP_HDR_STATS` (ondisk `HeapHdrStats`, workspace#499).

use std::collections::{HashMap, HashSet};

use fastmig_ondisk::Vpid;
use fastmig_ondisk::record::heap_hdr_stats::LineFields;

use crate::defect::{Check, Defect, OidText, VpidText};
use crate::heap_pass::{HeapPass, HeapTarget};

pub fn check_chain(t: &HeapTarget, p: &HeapPass, measured: bool) -> Vec<Defect> {
    let mut out = Vec::new();
    let d = |code| Defect::new(Check::HeapChain, code).class(t.name.clone());
    let links: HashMap<Vpid, _> = p.links.iter().map(|l| (l.vpid, *l)).collect();
    let in_file: HashSet<Vpid> = p.file_pages.iter().copied().collect();
    let mut visited = HashSet::new();
    let mut prev = Vpid::NULL;
    let mut at = t.hfid.vpid();
    while !at.is_null() {
        if !in_file.contains(&at) {
            out.push(
                d("chain-foreign-page")
                    .object(VpidText(at))
                    .at(format!("after {}", VpidText(prev))),
            );
            break;
        }
        if !visited.insert(at) {
            out.push(
                d("chain-cycle")
                    .object(VpidText(at))
                    .at(format!("after {}", VpidText(prev))),
            );
            break;
        }
        // a page whose slot 0 could not be read has a check 1 (ii) defect already; the walk stops there
        let Some(l) = links.get(&at) else { break };
        if l.prev != prev {
            out.push(
                d("chain-prev-link")
                    .object(VpidText(at))
                    .expected(VpidText(prev))
                    .actual(VpidText(l.prev)),
            );
        }
        prev = at;
        at = l.next;
    }
    let mut unvisited: Vec<_> = p
        .file_pages
        .iter()
        .filter(|v| !visited.contains(v))
        .collect();
    unvisited.sort();
    for v in unvisited {
        out.push(d("chain-unvisited-page").object(VpidText(*v)));
    }
    if let Some(h) = &p.header {
        if h.last_vpid != prev && !prev.is_null() {
            out.push(
                d("header-last-vpid")
                    .object(VpidText(t.hfid.vpid()))
                    .expected(VpidText(prev))
                    .actual(VpidText(h.last_vpid)),
            );
        }
        if let LineFields::Develop(dir) = &h.line {
            for e in dir.candidates.iter().filter(|e| !e.vpid.is_null()) {
                let what = match links.get(&e.vpid) {
                    // the header page holds records from slot 1 on; develop lists it as a candidate (T1)
                    _ if e.vpid == t.hfid.vpid() => continue,
                    Some(l) if l.bestspace => "a bestspace page",
                    Some(_) => continue,
                    None if in_file.contains(&e.vpid) => "a file page without a chain record",
                    None => "not a page of the heap file",
                };
                out.push(
                    d("bestspace-entry")
                        .object(VpidText(e.vpid))
                        .expected("a data page of the heap")
                        .actual(format!("{what} (freespace {})", e.freespace)),
                );
            }
        }
        if measured && (h.num_recs != p.num_recs || h.recs_sumlen != p.recs_sumlen) {
            out.push(
                d("header-stats")
                    .object(VpidText(t.hfid.vpid()))
                    .expected(format!(
                        "num_recs {} recs_sumlen {}",
                        p.num_recs, p.recs_sumlen
                    ))
                    .actual(format!(
                        "num_recs {} recs_sumlen {}",
                        h.num_recs, h.recs_sumlen
                    )),
            );
        }
    }
    let mut newhomes = p.newhomes.clone();
    newhomes.sort_unstable();
    let mut targets: Vec<_> = p.relocations.iter().map(|r| r.1).collect();
    targets.sort_unstable();
    for w in targets.windows(2).filter(|w| w[0] == w[1]) {
        out.push(d("newhome-shared").object(OidText(w[0])));
    }
    targets.dedup();
    // both sorted: one merge finds the orphans on each side
    let (mut i, mut j) = (0, 0);
    while i < newhomes.len() || j < targets.len() {
        match (newhomes.get(i), targets.get(j)) {
            (Some(a), Some(b)) if a == b => {
                i += 1;
                j += 1;
            }
            (Some(a), b) if b.is_none_or(|b| a < b) => {
                out.push(d("newhome-orphan").object(OidText(*a)));
                i += 1;
            }
            (_, Some(b)) => {
                out.push(d("relocation-dangling").object(OidText(*b)));
                j += 1;
            }
            _ => unreachable!(),
        }
    }
    out
}
