//! Where every piece of the regenerated catalog goes, and the catalog rows written there: new root-heap slots for the
//! new system classes and views, new heaps for the reallocated and new tables, the new catalog file and the record
//! each class's directory, class info and disk representation take in it, and every system table row in develop
//! layout. Class objects are written later ([`crate::catalog_write`]), once the index rebuild has given the BTIDs
//! they name.
//!
//! Input: the target's [`SpaceManager`] after [`crate::free_old_files`] and the user heaps' page treatment, the
//! [`SourceCatalog`], the catalog template, the source's vacuum horizon, the migrate start time and the user heap
//! statistics. Output: [`CatalogLayout`]; the root heap, the same-slot heaps and the new heaps in develop format with
//! their rows; the catalog file allocated (its pages are written by `catalog_write`); the HA apply-info rows deleted
//! ([`crate::ha_apply_info`]).
//! Source: workspace#482 decisions 1 (roles), 6 (heap placement), 10 (order); workspace#504 (two passes: rows name
//! each other by OID and their lengths do not depend on OID values, so a first pass with placeholder OIDs sizes every
//! record, its slots are reserved, and a second pass writes the real records into them; class objects are reserved
//! with placeholder BTIDs of the longest text, so the real ones never grow; BTIDs come after the rows: the 1st session
//! decision of 2026-10-29); P1h call order `target_classes` → `ct_rows` → `same_slot_rows` → `auth_rows` →
//! `static_rows` → `encode_row` (workspace#498).

use std::collections::{BTreeMap, HashMap};
use std::io;

use fastmig_ondisk::boot_parm::BootDbParm;
use fastmig_ondisk::catalog_dir::{self, CatalogItem, ClsInfo, ReprDir, ReprItem};
use fastmig_ondisk::class_object::{ClassObject, class_properties, encode};
use fastmig_ondisk::file_alloc::SpaceManager;
use fastmig_ondisk::heap_rewrite::{
    HeapJob, HeapWriter, PageStats, VacuumHorizon, create_heap, rewrite_heap,
};
use fastmig_ondisk::value::{DecodeResult, Value};
use fastmig_ondisk::{Btid, FileAndPage, Hfid, Oid, VersionLine, Vfid, Vpid};

use crate::auth_rows::auth_rows;
use crate::catalog_write::template_index_btid;
use crate::class_rows::Row;
use crate::classify::ClassKind;
use crate::ct_rows::ct_rows;
use crate::disk_repr_build;
use crate::ha_apply_info;
use crate::record_map::{RecordMap, Unmapped};
use crate::row_encode::encode_row;
use crate::same_slot_rows::{
    SameSlotInputs, name_in, same_slot_rows, template_line, template_rows,
};
use crate::source_catalog::SourceCatalog;
use crate::static_rows::static_rows;
use crate::system_classes::{SystemClassRole, same_slot};
use crate::system_rows::user;
use crate::system_table::SystemTable;
use crate::target_classes::{Placement, TargetClass, target_classes};
use crate::template::CatalogTemplate;

/// Volume id of placeholder OIDs and HFIDs: no database has this many volumes.
const PLACEHOLDER_VOLID: i16 = i16::MAX;
/// The BTID with the longest constraint text (`volid|fileid|root_pageid`, all non-negative in a real BTID).
pub const PLACEHOLDER_BTID: Btid = FileAndPage {
    vfid: Vfid {
        volid: i16::MAX,
        fileid: i32::MAX,
    },
    pageid: i32::MAX,
};

/// Develop tables whose records carry no MVCC header (`mvcc.c` `mvcc_is_mvcc_disabled_class`).
pub fn mvcc_table(table: SystemTable) -> bool {
    !matches!(
        table,
        SystemTable::DbSerial | SystemTable::DbCollation | SystemTable::DbHaApplyInfo
    )
}

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

fn decode_err(what: &str) -> impl Fn(fastmig_ondisk::value::DecodeError) -> io::Error + '_ {
    move |e| invalid(format!("{what}: {e}"))
}

/// The catalog records of one class, by OID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CatalogRecords {
    pub disk_repr: Oid,
    pub dir: Oid,
    pub cls_info: Oid,
}

/// Where the regenerated catalog lives. Placements are by name where the first pass did not know the class OID.
pub struct CatalogLayout {
    /// The target line: the format of the class objects and catalog records written (workspace#533).
    pub line: VersionLine,
    /// New root-heap slot of a template class, by name.
    pub new_slots: HashMap<Vec<u8>, Oid>,
    /// New heap of a reallocated or new table, by name.
    pub new_heaps: HashMap<Vec<u8>, Hfid>,
    /// Template user row → target user row.
    pub users: HashMap<Oid, Oid>,
    /// Catalog records per target class OID.
    pub records: HashMap<Oid, CatalogRecords>,
    /// Target class OIDs in catalog file order.
    pub class_order: Vec<Oid>,
    /// Statistics of every heap the catalog names (user heaps from the page treatment, system heaps written here).
    pub heap_stats: HashMap<Hfid, PageStats>,
    pub catalog_vfid: Vfid,
    pub catalog_pages: Vec<Vpid>,
    /// The catalog file's records as last written (`catalog_write` fills them).
    pub catalog_items: Vec<CatalogItem>,
    /// Rows written per table.
    pub rows_written: BTreeMap<&'static str, usize>,
    /// Class objects deleted from the root heap (11.4 views and dropped tables).
    pub deleted_classes: Vec<Oid>,
}

impl CatalogLayout {
    /// Nothing placed yet, for a target of `line`.
    pub fn empty(line: VersionLine) -> Self {
        CatalogLayout {
            line,
            new_slots: HashMap::new(),
            new_heaps: HashMap::new(),
            users: HashMap::new(),
            records: HashMap::new(),
            class_order: Vec::new(),
            heap_stats: HashMap::new(),
            catalog_vfid: Vfid::NULL,
            catalog_pages: Vec::new(),
            catalog_items: Vec::new(),
            rows_written: BTreeMap::new(),
            deleted_classes: Vec::new(),
        }
    }
}

/// One pass's placement answers: placeholders in the first pass, real places in the second.
/// One flag, not a type per pass: both passes run the same `generate`, and they differ only in where an OID, HFID or
/// directory comes from — seven two-arm choices inside closures `generate` hands to the row builders. A pass type
/// would turn each into a match on the same two arms; the first-pass lists below are only touched in the first arm
/// (workspace#525).
struct Places<'a> {
    first_pass: bool,
    layout: &'a CatalogLayout,
    /// First pass: placeholder OID → the class name it stands for (new slots).
    slot_names: Vec<(Oid, Vec<u8>)>,
    /// First pass: (class OID as given, class name) of each new heap asked for.
    heap_names: Vec<(Oid, Vec<u8>)>,
    /// First pass: tables in the order rows were allocated; second pass: the real OIDs to hand out in that order.
    allocs: Vec<(SystemTable, Oid)>,
    next_alloc: usize,
    placeholders: i32,
    /// First pass: placeholder user row → the template user row it stands for (INFORMATION_SCHEMA).
    new_users: Vec<(Oid, Oid)>,
    /// Constraints keep a BTID of their own (source BTIDs, template ones tagged) instead of the placeholder, so the
    /// index rebuild's definitions can be formed (the pair count); the run's passes take the placeholder.
    distinct_btids: bool,
}

impl<'a> Places<'a> {
    fn first_pass(empty: &'a CatalogLayout) -> Self {
        Places {
            first_pass: true,
            layout: empty,
            slot_names: Vec::new(),
            heap_names: Vec::new(),
            allocs: Vec::new(),
            next_alloc: 0,
            placeholders: 0,
            new_users: Vec::new(),
            distinct_btids: false,
        }
    }

    fn placeholder(&mut self) -> Oid {
        self.placeholders += 1;
        Oid::new(PLACEHOLDER_VOLID, self.placeholders, 0)
    }

    fn alloc(&mut self, table: SystemTable) -> Oid {
        if self.first_pass {
            let o = self.placeholder();
            self.allocs.push((table, o));
            return o;
        }
        let (t, o) = self.allocs[self.next_alloc];
        debug_assert_eq!(
            t, table,
            "the second pass allocates in the first pass's order"
        );
        self.next_alloc += 1;
        o
    }
}

/// Target classes and rows of one pass.
pub struct Generated {
    pub targets: Vec<TargetClass>,
    pub rows: Vec<(SystemTable, Row)>,
}

/// The first pass alone, writing nothing: every target class and catalog row with placeholder OIDs and HFIDs, each
/// value at its final length, constraints with distinct BTIDs (the plan's pair count of the system indexes,
/// workspace#526).
pub fn generated_rows(
    source: &SourceCatalog,
    template: &CatalogTemplate,
    now: &Value,
) -> DecodeResult<Generated> {
    let empty = CatalogLayout::empty(template.line().unwrap_or(VersionLine::Develop));
    let mut places = Places {
        distinct_btids: true,
        ..Places::first_pass(&empty)
    };
    generate(source, template, now, &mut places)
}

fn generate(
    source: &SourceCatalog,
    template: &CatalogTemplate,
    now: &Value,
    places: &mut Places,
) -> DecodeResult<Generated> {
    let line = template_line(template)?;
    let tusers = template_rows(template, SystemTable::DbUser)?;
    let first = places.first_pass;
    let distinct_btids = places.distinct_btids;
    // template users: the source user of the same name, else a new row
    let mut user_map: HashMap<Oid, Oid> = HashMap::new();
    for t in &tusers {
        let name = user::name(&t.values);
        let to = source.same_slot[&SystemTable::DbUser]
            .iter()
            .find(|u| user::name(&u.values) == name)
            .map(|u| u.oid);
        let to = match (to, first) {
            (Some(o), _) => o,
            (None, true) => places.placeholder(),
            (None, false) => *places
                .layout
                .users
                .get(&t.oid)
                .ok_or_else(|| decode_error("template user without a place"))?,
        };
        user_map.insert(t.oid, to);
    }
    let user = |o: Oid| *user_map.get(&o).unwrap_or(&o);
    let layout = places.layout;
    let targets = {
        let mut new_slot = |name: &[u8]| {
            if first {
                let o = places.placeholder();
                places.slot_names.push((o, name.to_vec()));
                Ok(o)
            } else {
                placed_slot(layout, name)
            }
        };
        let mut heap_names = Vec::new();
        let mut new_heap = |oid: Oid, name: &[u8]| {
            if first {
                heap_names.push((oid, name.to_vec()));
                Ok(FileAndPage {
                    vfid: Vfid::new(PLACEHOLDER_VOLID, heap_names.len() as i32),
                    pageid: 0,
                })
            } else {
                placed_heap(layout, name)
            }
        };
        // the first pass has no catalog records yet: a NULL directory OID has the real one's length
        let rep_dir = |oid: Oid| {
            if first {
                Ok(Oid::NULL)
            } else {
                placed_rep_dir(layout, oid)
            }
        };
        let placeholder = |_| Some(PLACEHOLDER_BTID);
        let source_btid: &dyn Fn(Btid) -> Option<Btid> =
            if distinct_btids { &Some } else { &placeholder };
        let template_btid: &dyn Fn(Btid) -> Option<Btid> = if distinct_btids {
            &template_index_btid
        } else {
            &placeholder
        };
        let t = target_classes(
            &source.classes,
            &source.plans,
            source.version,
            template,
            &mut Placement {
                new_slot: &mut new_slot,
                new_heap: &mut new_heap,
                rep_dir: &rep_dir,
                source_btid,
                template_btid,
                user: &user,
            },
        )?;
        if first {
            places.heap_names = heap_names;
        }
        t
    };
    let mut alloc = |t: SystemTable| places.alloc(t);
    let mut rows = ct_rows(&targets, line, now, &mut alloc)?;
    let same = same_slot_rows(
        &SameSlotInputs {
            source: &source.same_slot,
            targets: &targets,
            template,
            user: &user,
            charset: source.charset,
            now: now.clone(),
        },
        &mut alloc,
    )?;
    let auths: Vec<&Row> = same
        .iter()
        .filter(|(t, _)| *t == SystemTable::DbAuthorization)
        .map(|(_, r)| r)
        .collect();
    let auth = auth_rows(&auths, line, now, &mut alloc)?;
    rows.extend(same);
    rows.extend(auth);
    let src = |t: SystemTable| {
        source
            .static_source
            .get(&t)
            .map(|rs| {
                rs.iter()
                    .map(|r| Row {
                        oid: r.oid,
                        values: r.values.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    rows.extend(static_rows(&src, &targets, template, now, &mut alloc)?);
    if first {
        places.new_users = user_map
            .into_iter()
            .filter(|(_, o)| is_placeholder(*o))
            .map(|(t, o)| (o, t))
            .collect();
    }
    Ok(Generated { targets, rows })
}

fn decode_error(what: &str) -> fastmig_ondisk::value::DecodeError {
    fastmig_ondisk::value::DecodeError {
        at: 0,
        what: what.to_string(),
    }
}

fn is_placeholder(o: Oid) -> bool {
    o.volid == PLACEHOLDER_VOLID
}

/// The target class a row of `table` is encoded with.
fn table_class<'t>(targets: &'t [TargetClass], table: &str) -> io::Result<&'t TargetClass> {
    targets
        .iter()
        .find(|t| t.class.name == table.as_bytes())
        .ok_or_else(|| invalid(format!("no target class {table}")))
}

/// Per attribute id, the indexes whose first key it is (engine `or_install_btids_constraint`), in the engine's
/// family order FK, PK, U, RU, I, RI (`or_install_btids`); a constraint sharing another's index has none. `c` is of
/// `line`.
pub fn attribute_btids(
    c: &ClassObject,
    line: VersionLine,
) -> DecodeResult<HashMap<i32, Vec<(Btid, bool)>>> {
    use fastmig_ondisk::class_object::ConstraintKind as K;
    let props = class_properties(&c.properties, line)?;
    let rank = |k: &K| match k {
        K::ForeignKey => 0,
        K::PrimaryKey => 1,
        K::Unique => 2,
        K::ReverseUnique => 3,
        K::Index => 4,
        K::ReverseIndex => 5,
    };
    let mut cons: Vec<_> = props
        .constraints
        .iter()
        .filter(|k| k.shared_with.is_none())
        .collect();
    cons.sort_by_key(|k| rank(&k.kind));
    let mut out: HashMap<i32, Vec<(Btid, bool)>> = HashMap::new();
    for k in cons {
        if let Some(&(first, _)) = k.keys.first() {
            out.entry(first)
                .or_default()
                .push((k.btid.unwrap_or(Btid::NULL), k.has_function));
        }
    }
    Ok(out)
}

/// The three catalog records of target class `t`: disk representation, directory, class info (the order a new
/// class gets them: `catalog_add_representation`, then `catalog_add_class_info`), in `line`'s layout.
pub fn class_catalog_items(
    t: &TargetClass,
    records: CatalogRecords,
    stats: PageStats,
    line: VersionLine,
) -> DecodeResult<[CatalogItem; 3]> {
    let btids = attribute_btids(&t.class, line)?;
    let disk = disk_repr_build::build(&t.class, line, |id| {
        btids.get(&id).cloned().unwrap_or_default()
    })?;
    let dir = ReprDir {
        items: vec![
            ReprItem {
                repr_id: catalog_dir::NULL_REPRID,
                record: records.cls_info,
            },
            ReprItem {
                repr_id: t.class.repid as i16,
                record: records.disk_repr,
            },
        ],
    };
    let info = ClsInfo {
        hfid: t.class.heap,
        tot_pages: stats.pages.min(i32::MAX as u64) as i32,
        tot_objs: stats.recs as i64,
        time_stamp: 0,
        rep_dir: records.dir,
    };
    Ok([
        CatalogItem::DiskRepr(disk),
        CatalogItem::Dir(dir),
        CatalogItem::ClsInfo(info),
    ])
}

/// Places the regenerated catalog and writes its rows (see the module header).
pub fn plan_catalog(
    space: &SpaceManager,
    source: &SourceCatalog,
    template: &CatalogTemplate,
    horizon: &VacuumHorizon,
    now: &Value,
    user_heap_stats: HashMap<Hfid, PageStats>,
) -> io::Result<CatalogLayout> {
    let vs = space.volumes();
    let (_, boot) = BootDbParm::read(vs)?;
    let line = template_line(template).map_err(decode_err("catalog template"))?;
    let mut layout = CatalogLayout {
        heap_stats: user_heap_stats,
        ..CatalogLayout::empty(line)
    };

    // first pass: placeholders everywhere, real lengths
    let empty = CatalogLayout::empty(line);
    let mut p1 = Places::first_pass(&empty);
    let g1 = generate(source, template, now, &mut p1).map_err(decode_err("catalog first pass"))?;

    // root heap: the target format with every kept class object at its final length, then deletions and new slots
    let class_bytes: HashMap<Oid, Vec<u8>> = g1
        .targets
        .iter()
        .map(|t| (t.oid, encode(&t.class, line)))
        .collect();
    let root = boot.rootclass_hfid;
    rewrite_heap(
        space,
        horizon,
        &HeapJob {
            hfid: root,
            source_line: source.line,
            re_encode: Some(&RecordMap {
                records: class_bytes
                    .iter()
                    .filter(|(o, _)| !is_placeholder(**o))
                    .map(|(o, b)| (*o, b.clone()))
                    .collect(),
                mvcc: false,
                unmapped: Unmapped::Keep,
            }),
            root: true,
            pages_walked: None,
            isolated: None,
        },
    )
    .map_err(|e| invalid(format!("root heap: {e}")))?;
    let mut rw = HeapWriter::open(space, root)?;
    for (c, plan) in source.classes.iter().zip(&source.plans) {
        if matches!(
            plan.kind,
            ClassKind::System(SystemClassRole::Dropped | SystemClassRole::View)
        ) {
            rw.delete(c.oid)?;
            layout.deleted_classes.push(c.oid);
        }
    }
    let mut real_class: HashMap<Oid, Oid> = HashMap::new();
    for (placeholder, name) in &p1.slot_names {
        let oid = rw.insert(&class_bytes[placeholder], false)?;
        layout.new_slots.insert(name.clone(), oid);
        real_class.insert(*placeholder, oid);
    }
    let root_stats = rw.finish()?;
    layout.heap_stats.insert(root, root_stats);

    // new heaps of reallocated and new tables, under their real class OIDs
    for (oid, name) in &p1.heap_names {
        let class_oid = real_class.get(oid).copied().unwrap_or(*oid);
        layout
            .new_heaps
            .insert(name.clone(), create_heap(space, class_oid, false)?);
    }

    // catalog file: sized from the first pass's classes, allocated, positions read back
    let sizing: Vec<CatalogItem> = g1
        .targets
        .iter()
        .map(|t| {
            class_catalog_items(
                t,
                CatalogRecords {
                    disk_repr: Oid::NULL,
                    dir: Oid::NULL,
                    cls_info: Oid::NULL,
                },
                PageStats::default(),
                layout.line,
            )
        })
        .collect::<DecodeResult<Vec<_>>>()
        .map_err(decode_err("catalog records"))?
        .into_iter()
        .flatten()
        .collect();
    let io_size = vs.io_pagesize();
    let sized = catalog_dir::build_pages(&sizing, layout.line, io_size, &[]);
    let (vfid, vpids) = catalog_dir::create_file(space, sized.pages.len())?;
    let at = |i: usize| {
        let (p, s) = sized.at[i];
        Oid::new(vpids[p].volid, vpids[p].pageid, s as i16)
    };
    for (k, t) in g1.targets.iter().enumerate() {
        let oid = real_class.get(&t.oid).copied().unwrap_or(t.oid);
        layout.records.insert(
            oid,
            CatalogRecords {
                disk_repr: at(3 * k),
                dir: at(3 * k + 1),
                cls_info: at(3 * k + 2),
            },
        );
        layout.class_order.push(oid);
    }
    layout.catalog_vfid = vfid;
    layout.catalog_pages = vpids;
    layout.catalog_items = sizing;

    // row heaps: the same-slot tables in the target format first (first-pass rows at their final lengths)
    let encode = |targets: &[TargetClass], t: &SystemTable, r: &Row| -> io::Result<Vec<u8>> {
        encode_row(&table_class(targets, table_name(*t, line)?)?.class, line, r)
            .map_err(|e| invalid(format!("{t} {:?}: {e}", r.oid)))
    };
    let mut writers: HashMap<Vec<u8>, HeapWriter> = HashMap::new();
    for dev in same_slot(source.version) {
        let src_name = dev.source_name().unwrap_or_default();
        let sc = source
            .classes
            .iter()
            .find(|c| c.class.name == src_name.as_bytes())
            .ok_or_else(|| invalid(format!("no source class {src_name}")))?;
        if sc.class.heap.is_null() {
            continue;
        }
        let records = g1
            .rows
            .iter()
            .filter(|(t, r)| *t == dev && !is_placeholder(r.oid))
            .map(|(t, r)| Ok((r.oid, encode(&g1.targets, t, r)?)))
            .collect::<io::Result<HashMap<_, _>>>()?;
        rewrite_heap(
            space,
            horizon,
            &HeapJob {
                hfid: sc.class.heap,
                source_line: source.line,
                re_encode: Some(&RecordMap {
                    records,
                    mvcc: mvcc_table(dev),
                    unmapped: Unmapped::Refuse,
                }),
                root: false,
                pages_walked: None,
                isolated: None,
            },
        )
        .map_err(|e| invalid(format!("{dev} heap: {e}")))?;
        writers.insert(
            table_name(dev, line)?.as_bytes().to_vec(),
            HeapWriter::open(space, sc.class.heap)?,
        );
    }
    for (name, hfid) in &layout.new_heaps {
        writers.insert(name.clone(), HeapWriter::open(space, *hfid)?);
    }

    // reserve the slot of every row the first pass gave a placeholder OID
    let mut real_row: HashMap<Oid, Oid> = HashMap::new();
    for (t, r) in &g1.rows {
        if is_placeholder(r.oid) {
            let bytes = encode(&g1.targets, t, r)?;
            let oid = writer(&mut writers, t, line)?.insert(&bytes, mvcc_table(*t))?;
            real_row.insert(r.oid, oid);
        }
    }
    for (placeholder, template_user) in &p1.new_users {
        let oid = real_row
            .get(placeholder)
            .ok_or_else(|| invalid("new user without a row".into()))?;
        layout.users.insert(*template_user, *oid);
    }

    // second pass: real OIDs; every row written in its slot
    let allocs = p1
        .allocs
        .iter()
        .map(|(t, o)| {
            real_row
                .get(o)
                .map(|r| (*t, *r))
                .ok_or_else(|| invalid(format!("{t}: an allocated OID without a row")))
        })
        .collect::<io::Result<Vec<_>>>()?;
    let mut p2 = Places {
        first_pass: false,
        layout: &layout,
        slot_names: Vec::new(),
        heap_names: Vec::new(),
        allocs,
        next_alloc: 0,
        placeholders: 0,
        new_users: Vec::new(),
        distinct_btids: false,
    };
    let g2 = generate(source, template, now, &mut p2).map_err(decode_err("catalog second pass"))?;
    let mut written: BTreeMap<&'static str, usize> = BTreeMap::new();
    // the HA apply-info rows are not carried: deleted from their slots (crate::ha_apply_info, workspace#590)
    let mut ha_rows = Vec::new();
    for (t, r) in &g2.rows {
        if *t == SystemTable::DbHaApplyInfo {
            ha_rows.push(r.oid);
            continue;
        }
        writer(&mut writers, t, line)?.place(r.oid, &encode(&g2.targets, t, r)?, mvcc_table(*t))?;
        *written.entry(table_name(*t, line)?).or_default() += 1;
    }
    // recorded when 0 too, as on the same-version path
    let reset = if ha_rows.is_empty() {
        0
    } else {
        let w = writer(&mut writers, &SystemTable::DbHaApplyInfo, line)?;
        ha_apply_info::reset(w, &ha_rows)?
    };
    written.insert(ha_apply_info::RESET_ROWS, reset);
    for (_, w) in writers {
        let hfid = w.hfid();
        let stats = w.finish()?;
        layout.heap_stats.insert(hfid, stats);
    }
    layout.rows_written = written;
    Ok(layout)
}

/// The open heap of table `t`'s rows, keyed by its name in `line`.
fn writer<'m, 's, 'v>(
    writers: &'m mut HashMap<Vec<u8>, HeapWriter<'s, 'v>>,
    t: &SystemTable,
    line: VersionLine,
) -> io::Result<&'m mut HeapWriter<'s, 'v>> {
    writers
        .get_mut(table_name(*t, line)?.as_bytes())
        .ok_or_else(|| invalid(format!("no heap for rows of {t}")))
}

/// The name of `t` in the target line's catalog.
fn table_name(t: SystemTable, line: VersionLine) -> io::Result<&'static str> {
    name_in(line, t).map_err(decode_err("system table"))
}

/// The root-heap slot the layout gave the template class `name`; every new class has one after the first pass.
pub(crate) fn placed_slot(layout: &CatalogLayout, name: &[u8]) -> DecodeResult<Oid> {
    layout.new_slots.get(name).copied().ok_or_else(|| {
        decode_error(&format!(
            "class {} has no root-heap slot in the layout",
            String::from_utf8_lossy(name)
        ))
    })
}

/// The heap the layout created for table `name`; every reallocated or new table has one after the first pass.
pub(crate) fn placed_heap(layout: &CatalogLayout, name: &[u8]) -> DecodeResult<Hfid> {
    layout.new_heaps.get(name).copied().ok_or_else(|| {
        decode_error(&format!(
            "table {} has no heap in the layout",
            String::from_utf8_lossy(name)
        ))
    })
}

/// The representation directory of target class `oid` in the catalog file; every target class has one.
pub(crate) fn placed_rep_dir(layout: &CatalogLayout, oid: Oid) -> DecodeResult<Oid> {
    layout
        .records
        .get(&oid)
        .map(|r| r.dir)
        .ok_or_else(|| decode_error(&format!("class {oid:?} has no catalog record")))
}
