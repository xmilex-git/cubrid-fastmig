//! One sector-parallel pass over one heap file: every page and record is read once, and everything the checks need
//! from the heap comes out of that pass.
//!
//! Input: the database, the heap ([`HeapTarget`]), the indexes whose keys come from its rows ([`IndexKeys`]), and
//! the shared worker and memory limits. Output: [`HeapPass`] — the page links for the chain walk (`heap_chain`),
//! check 1 defects of the pages and records (slot directory, chain record class, record homes, MVCC residue in a
//! target, record decode), the live OIDs, the measured `num_recs`/`recs_sumlen`, the RELOCATION → NEWHOME pairs, the
//! OBJECT references and LOB locators of live rows, and per index the sorted-run input of the (key, OID) merge.
//! Source: G3 workspace#483 Q5 (check 1 (ii)–(v)) and Q9 (sector-range parallel scan, one class scan yields the keys
//! of all its indexes); the scan driver is the index builder's (`fastmig_index::heap_scan::scan_class`,
//! workspace#493); record homes `heap_file.c` (`ondisk::record::resolve_record`); slotted page `slotted_page.c`
//! `spage_check` (free-space counters).

use std::collections::HashMap;
use std::sync::Mutex;

use fastmig_index::BuildError;
use fastmig_index::heap_scan::{ClassRuns, RowKey, scan_class};
use fastmig_index::{KeyOrder, MemoryLimit, SpillDir, WorkerLimit};
use fastmig_ondisk::class_object::{ClassObject, Constraint};
use fastmig_ondisk::file_table::FileTable;
use fastmig_ondisk::page::{self, PageType};
use fastmig_ondisk::record::heap_chain::{FLAG_VACUUM_ONCE, FLAG_VACUUM_STATUS_MASK, HeapChain};
use fastmig_ondisk::record::{
    ExtractedKey, HeapHdrStats, RecordHeader, RecordLayout, decode_record, extract_key,
    resolve_record,
};
use fastmig_ondisk::spage::{RecordType, SPAGE_HEADER_SIZE, SPAGE_SLOT_SIZE, SlottedPage};
use fastmig_ondisk::value::{KeyDomain, Value};
use fastmig_ondisk::{Hfid, Oid, Vpid};

use crate::database::{Database, Role};
use crate::defect::{Check, Defect, OidText, VpidText};
use crate::object_refs::{LobRef, ObjectRef, collect_refs};

/// The heap to scan. `class` is `None` for the root heap, whose records are class objects (decoded at open).
pub struct HeapTarget<'a> {
    pub name: String,
    pub class_oid: Oid,
    pub class: Option<&'a ClassObject>,
    pub hfid: Hfid,
}

/// One index whose keys this heap's live rows produce.
pub struct IndexKeys<'a> {
    pub constraint: &'a Constraint,
    pub kd: &'a KeyDomain,
}

/// Slot 0 of one page: what the chain walk needs.
#[derive(Clone, Copy, Debug)]
pub struct PageLink {
    pub vpid: Vpid,
    pub prev: Vpid,
    pub next: Vpid,
    pub bestspace: bool,
}

pub struct HeapPass<'m> {
    /// Every user page of the heap file, ascending.
    pub file_pages: Vec<Vpid>,
    pub links: Vec<PageLink>,
    pub header: Option<HeapHdrStats>,
    pub live: Vec<Oid>,
    pub num_recs: u64,
    pub recs_sumlen: u64,
    /// (home, newhome) of every REC_RELOCATION.
    pub relocations: Vec<(Oid, Oid)>,
    /// Every REC_NEWHOME slot.
    pub newhomes: Vec<Oid>,
    pub refs: Vec<ObjectRef>,
    pub lobs: Vec<LobRef>,
    /// Per index (in `IndexKeys` order): sorted-run input and all-NULL key rows.
    pub runs: ClassRuns<'m>,
    pub defects: Vec<Defect>,
}

#[derive(Default)]
struct Shared {
    links: Vec<PageLink>,
    header: Option<HeapHdrStats>,
    live: Vec<Oid>,
    num_recs: u64,
    recs_sumlen: u64,
    relocations: Vec<(Oid, Oid)>,
    newhomes: Vec<Oid>,
    refs: Vec<ObjectRef>,
    lobs: Vec<LobRef>,
    defects: Vec<Defect>,
}

pub fn scan_heap<'m>(
    db: &Database,
    t: &HeapTarget,
    indexes: &[IndexKeys],
    workers: &WorkerLimit,
    memory: &'m MemoryLimit,
    spill: &SpillDir,
) -> Result<HeapPass<'m>, BuildError> {
    let ft = FileTable::read(&db.vols, t.hfid.vfid)?;
    let sectors = ft.user_pages_by_sector();
    let file_pages: Vec<Vpid> = ft.user_pages().into_iter().collect();
    let layouts = t.class.map(|c| layouts_of(db, c)).unwrap_or_default();
    let orders: Vec<&dyn KeyOrder> = indexes.iter().map(|i| i.kd as &dyn KeyOrder).collect();
    let shared = Mutex::new(Shared::default());
    let runs = scan_class(&sectors, &orders, workers, memory, spill, |vpid, emit| {
        let mut out = Shared::default();
        let page = db.vols.read_page(vpid)?;
        scan_page(db, t, indexes, &layouts, vpid, &page, &mut out, emit);
        let mut s = shared.lock().unwrap();
        let o = out;
        s.links.extend(o.links);
        if o.header.is_some() {
            s.header = o.header;
        }
        s.live.extend(o.live);
        s.num_recs += o.num_recs;
        s.recs_sumlen += o.recs_sumlen;
        s.relocations.extend(o.relocations);
        s.newhomes.extend(o.newhomes);
        s.refs.extend(o.refs);
        s.lobs.extend(o.lobs);
        s.defects.extend(o.defects);
        Ok(())
    })?;
    let s = shared.into_inner().unwrap();
    Ok(HeapPass {
        file_pages,
        links: s.links,
        header: s.header,
        live: s.live,
        num_recs: s.num_recs,
        recs_sumlen: s.recs_sumlen,
        relocations: s.relocations,
        newhomes: s.newhomes,
        refs: s.refs,
        lobs: s.lobs,
        runs,
        defects: s.defects,
    })
}

/// The layout of every representation the class still knows (records may carry an old repid).
fn layouts_of(db: &Database, c: &ClassObject) -> HashMap<u32, Result<RecordLayout, String>> {
    std::iter::once(c.repid)
        .chain(c.representations.iter().map(|r| r.id))
        .map(|id| {
            let l = RecordLayout::for_repid(db.line, c, id as u32).map_err(|e| e.to_string());
            (id as u32, l)
        })
        .collect()
}

fn d(t: &HeapTarget, code: &'static str, vpid: Vpid) -> Defect {
    Defect::new(Check::HeapChain, code)
        .class(t.name.clone())
        .at(format!("vpid {}", VpidText(vpid)))
}

#[allow(clippy::too_many_arguments)]
fn scan_page(
    db: &Database,
    t: &HeapTarget,
    indexes: &[IndexKeys],
    layouts: &HashMap<u32, Result<RecordLayout, String>>,
    vpid: Vpid,
    page: &[u8],
    o: &mut Shared,
    emit: &mut dyn FnMut(usize, Oid, RowKey),
) {
    if page::ptype(page) != Some(PageType::Heap) || page::vpid(page) != vpid {
        o.defects.push(
            d(t, "page-header", vpid)
                .object(VpidText(vpid))
                .expected("heap page with its own vpid")
                .actual(format!(
                    "ptype {} vpid {}",
                    page::ptype_raw(page),
                    VpidText(page::vpid(page))
                )),
        );
        return;
    }
    let Some(sp) = SlottedPage::new(page::db_area(page)) else {
        o.defects
            .push(d(t, "slotted-page-header", vpid).object(VpidText(vpid)));
        return;
    };
    check_slots(t, vpid, &sp, o);
    // the root heap was created before the root class OID existed: its pages carry a NULL class OID (T1, both lines)
    let want_class = if t.class.is_some() {
        t.class_oid
    } else {
        Oid::NULL
    };
    let Some((_, slot0)) = sp.record(0) else {
        o.defects
            .push(d(t, "no-slot-0", vpid).object(VpidText(vpid)));
        return;
    };
    if vpid == t.hfid.vpid() {
        match HeapHdrStats::read(slot0, db.line) {
            Ok(h) => {
                if h.class_oid != want_class {
                    o.defects.push(
                        d(t, "header-class-oid", vpid)
                            .object(VpidText(vpid))
                            .expected(OidText(want_class))
                            .actual(OidText(h.class_oid)),
                    );
                }
                o.links.push(PageLink {
                    vpid,
                    prev: Vpid::NULL,
                    next: h.next_vpid,
                    bestspace: false,
                });
                o.header = Some(h);
            }
            Err(e) => o.defects.push(
                d(t, "header-stats-decode", vpid)
                    .object(VpidText(vpid))
                    .actual(e),
            ),
        }
    } else {
        let Some(chain) = HeapChain::read(slot0) else {
            o.defects.push(
                d(t, "chain-record", vpid)
                    .object(VpidText(vpid))
                    .expected("40 B HEAP_CHAIN")
                    .actual(format!("{} B", slot0.len())),
            );
            return;
        };
        if chain.class_oid != want_class {
            o.defects.push(
                d(t, "chain-class-oid", vpid)
                    .object(VpidText(vpid))
                    .expected(OidText(want_class))
                    .actual(OidText(chain.class_oid)),
            );
        }
        // the target starts with no vacuum residue (G3 Q5 (v)): patch targets were reset, the other pages kept
        // their chain and must look vacuumed with every MVCCID before the new log's (workspace#499, #501)
        if db.role == Role::Target {
            let status = chain.flags & FLAG_VACUUM_STATUS_MASK;
            let residue = status == FLAG_VACUUM_ONCE
                || chain.max_mvccid >= db.mvcc_next_id
                || db
                    .source_horizon
                    .is_some_and(|h| h.pending && h.page_needs_patch(&chain));
            if residue {
                o.defects.push(
                    d(t, "page-vacuum-residue", vpid)
                        .object(VpidText(vpid))
                        .expected(format!(
                            "vacuum status not ONCE, max_mvccid < {}{}",
                            db.mvcc_next_id,
                            db.source_horizon.filter(|h| h.pending).map_or(
                                String::new(),
                                |h| format!(
                                    ", UNKNOWN only below the source's oldest unvacuumed {}",
                                    h.oldest_unvacuumed
                                )
                            )
                        ))
                        .actual(format!(
                            "max_mvccid {} flags {:#x}",
                            chain.max_mvccid, chain.flags
                        )),
                );
            }
        }
        o.links.push(PageLink {
            vpid,
            prev: chain.prev_vpid,
            next: chain.next_vpid,
            bestspace: chain.is_bestspace(),
        });
        if chain.is_bestspace() {
            return;
        }
    }
    for slot in 1..sp.num_slots() {
        let Some((rtype, rec)) = sp.record(slot) else {
            continue;
        };
        let oid = Oid::new(vpid.volid, vpid.pageid, slot as i16);
        match rtype {
            RecordType::NewHome => {
                o.newhomes.push(oid);
                continue;
            }
            RecordType::Home | RecordType::Relocation | RecordType::BigOne => {}
            _ => continue,
        }
        let r = match resolve_record(&db.vols, oid, rtype, rec) {
            Ok(Some(r)) => r,
            Ok(None) => continue,
            Err(e) => {
                o.defects.push(
                    d(t, "record-home", vpid)
                        .object(OidText(oid))
                        .expected(format!("{rtype:?} resolving to a record"))
                        .actual(e),
                );
                continue;
            }
        };
        if let fastmig_ondisk::record::HomeKind::Relocated { newhome } = r.kind {
            o.relocations.push((oid, newhome));
        }
        let Some(class) = t.class else {
            // root heap: class objects, not MVCC records
            o.live.push(oid);
            o.num_recs += 1;
            o.recs_sumlen += r.bytes.len() as u64;
            continue;
        };
        let header = match RecordHeader::read(&r.bytes) {
            Ok(h) => h,
            Err(e) => {
                o.defects
                    .push(d(t, "record-header", vpid).object(OidText(oid)).actual(e));
                continue;
            }
        };
        if db.role == Role::Target && header.mvcc_flags != 0 {
            o.defects.push(
                d(t, "record-mvcc-residue", vpid)
                    .object(OidText(oid))
                    .expected("no MVCC flags")
                    .actual(format!("flags {:#x}", header.mvcc_flags)),
            );
        }
        if header.is_deleted() {
            continue;
        }
        o.live.push(oid);
        o.num_recs += 1;
        o.recs_sumlen += r.bytes.len() as u64;
        let layout = match layouts.get(&header.repid) {
            Some(Ok(l)) => l,
            Some(Err(e)) => {
                o.defects.push(
                    d(t, "record-layout", vpid)
                        .object(OidText(oid))
                        .actual(format!("repid {}: {e}", header.repid)),
                );
                continue;
            }
            None => {
                o.defects.push(
                    d(t, "record-repid", vpid)
                        .object(OidText(oid))
                        .expected(format!(
                            "one of the class's repids (current {})",
                            class.repid
                        ))
                        .actual(header.repid),
                );
                continue;
            }
        };
        let values = match decode_record(db.line, layout, &r.bytes) {
            Ok(dr) => dr.values,
            Err(e) => {
                o.defects
                    .push(d(t, "record-decode", vpid).object(OidText(oid)).actual(e));
                continue;
            }
        };
        collect_refs(oid, layout, &values, &mut o.refs, &mut o.lobs);
        row_keys(t, indexes, layout, &values, oid, o, emit);
    }
}

fn row_keys(
    t: &HeapTarget,
    indexes: &[IndexKeys],
    layout: &RecordLayout,
    values: &[Value],
    oid: Oid,
    o: &mut Shared,
    emit: &mut dyn FnMut(usize, Oid, RowKey),
) {
    for (ix, i) in indexes.iter().enumerate() {
        let name = String::from_utf8_lossy(&i.constraint.name).into_owned();
        match extract_key(i.kd, layout, values, i.constraint) {
            Ok(ExtractedKey::Key(k)) => emit(ix, oid, RowKey::Key(&k)),
            Ok(ExtractedKey::AllNull) => emit(ix, oid, RowKey::AllNull),
            // the builder refuses this row (R-ROW-4); here it is a defect of the row
            Ok(ExtractedKey::NullInPrimaryKey(col)) => o.defects.push(
                Defect::new(Check::IndexHeap, "null-in-primary-key")
                    .class(t.name.clone())
                    .index(name)
                    .object(OidText(oid))
                    .actual(format!("key column {col} is NULL")),
            ),
            Err(e) => o.defects.push(
                Defect::new(Check::IndexHeap, "key-extract")
                    .class(t.name.clone())
                    .index(name)
                    .object(OidText(oid))
                    .actual(e),
            ),
        }
    }
}

/// Slot directory: records inside the record area, no two overlapping, and the free-space counters as
/// `spage_check` computes them (aligned record lengths).
fn check_slots(t: &HeapTarget, vpid: Vpid, sp: &SlottedPage, o: &mut Shared) {
    let area = sp.area_len();
    let n = sp.num_slots();
    let dir_start = area - n * SPAGE_SLOT_SIZE;
    let align = sp.alignment().max(1);
    let mut extents: Vec<(usize, usize, usize)> = Vec::new();
    let mut used = 0usize;
    for slot in 0..n {
        let (off, len, _) = sp.raw_slot(slot).unwrap();
        if off == 0 {
            continue;
        }
        if off < SPAGE_HEADER_SIZE || off + len > dir_start {
            o.defects.push(
                d(t, "slot-out-of-area", vpid)
                    .object(format!("{} slot {slot}", VpidText(vpid)))
                    .expected(format!("{SPAGE_HEADER_SIZE}..{dir_start}"))
                    .actual(format!("{off}+{len}")),
            );
            continue;
        }
        used += len.div_ceil(align) * align;
        extents.push((off, len, slot));
    }
    extents.sort_unstable();
    for w in extents.windows(2) {
        if w[0].0 + w[0].1 > w[1].0 {
            o.defects.push(
                d(t, "slot-overlap", vpid)
                    .object(format!("{} slot {}", VpidText(vpid), w[1].2))
                    .actual(format!("slot {} ends at {}", w[0].2, w[0].0 + w[0].1)),
            );
        }
    }
    let total_free = (dir_start - SPAGE_HEADER_SIZE) as i64 - used as i64;
    if sp.total_free() as i64 != total_free {
        o.defects.push(
            d(t, "total-free", vpid)
                .object(VpidText(vpid))
                .expected(total_free)
                .actual(sp.total_free()),
        );
    }
    let cont_free = dir_start as i64 - sp.offset_to_free_area() as i64;
    if sp.cont_free() as i64 != cont_free {
        o.defects.push(
            d(t, "cont-free", vpid)
                .object(VpidText(vpid))
                .expected(cont_free)
                .actual(sp.cont_free()),
        );
    }
}
