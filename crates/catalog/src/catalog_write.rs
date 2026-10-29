//! Writes the regenerated class objects and the new catalog file once the index rebuild has created the B-tree files:
//! every class object in its root-heap slot with the new BTIDs, the catalog file's directory, class-info and
//! disk-representation records, and `BOOT_DB_PARM.ctid`. After the B-trees are filled, [`write_bt_stats`] puts
//! their statistics into the disk representations.
//!
//! Input: the target's [`SpaceManager`], the [`CatalogLayout`] [`crate::catalog_layout::plan_catalog`] made, the
//! [`SourceCatalog`], the catalog template and the new BTID of each index by (target class OID, constraint name).
//! Output: root heap, catalog file and boot record written; the layout's catalog records updated.
//! Source: workspace#482 decision 10.5 (class objects with the new BTIDs and rep_dir, then the catalog directory);
//! 1st session decision 2026-10-29 (BTIDs exist before the class objects are written, the B-trees are filled after
//! the system heaps are; BT_STATS are written in place afterwards, a disk representation keeps its length);
//! workspace#503 (the new BTIDs are keyed by class and constraint name: source and template BTIDs overlap).

use std::collections::HashMap;
use std::io;

use fastmig_ondisk::boot_parm::BootDbParm;
use fastmig_ondisk::catalog_dir::{self, BtStats, CatalogItem};
use fastmig_ondisk::class_object::{class_properties, decode, encode_develop};
use fastmig_ondisk::file_alloc::SpaceManager;
use fastmig_ondisk::heap_rewrite::HeapWriter;
use fastmig_ondisk::value::DecodeResult;
use fastmig_ondisk::{Btid, Hfid, Oid, VersionLine};

use crate::catalog_layout::{CatalogLayout, class_catalog_items};
use crate::classify::ClassKind;
use crate::same_slot_rows::template_rows;
use crate::source_catalog::SourceCatalog;
use crate::system_classes::SystemClassRole;
use crate::target_classes::{Placement, TargetClass, target_classes};
use crate::template::CatalogTemplate;

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

/// The new BTID of an index: (target class OID, constraint name) → BTID (`None` writes the NULL BTID).
pub type NewBtid<'a> = &'a dyn Fn(Oid, &[u8]) -> Option<Btid>;

/// Old BTID → new, for the source's user classes (`line` layout) and for the template's system classes.
fn btid_maps(
    source: &SourceCatalog,
    template: &CatalogTemplate,
    layout: &CatalogLayout,
    new_btid: NewBtid,
) -> DecodeResult<(HashMap<Btid, Btid>, HashMap<Btid, Btid>)> {
    let mut src = HashMap::new();
    let mut target_of: HashMap<Vec<u8>, Oid> = layout.new_slots.clone();
    for (c, p) in source.classes.iter().zip(&source.plans) {
        match &p.kind {
            ClassKind::User => {
                for k in class_properties(&c.class.properties, source.line)?.constraints {
                    if let (Some(old), Some(new)) = (k.btid, new_btid(c.oid, &k.name)) {
                        src.insert(old, new);
                    }
                }
            }
            ClassKind::System(SystemClassRole::SameSlot { develop_name }) => {
                target_of.insert(develop_name.as_bytes().to_vec(), c.oid);
            }
            ClassKind::System(SystemClassRole::Reallocated) => {
                target_of.insert(c.class.name.clone(), c.oid);
            }
            ClassKind::System(_) => {}
        }
    }
    let mut tpl = HashMap::new();
    for r in &template.classes {
        let t = decode(&r.bytes, VersionLine::Develop)?;
        let Some(&oid) = target_of.get(&t.name) else {
            continue;
        };
        for k in class_properties(&t.properties, VersionLine::Develop)?.constraints {
            if let (Some(old), Some(new)) = (k.btid, new_btid(oid, &k.name)) {
                tpl.insert(old, new);
            }
        }
    }
    Ok((src, tpl))
}

/// The target classes at their final places with the new BTIDs.
fn final_targets(
    source: &SourceCatalog,
    template: &CatalogTemplate,
    layout: &CatalogLayout,
    new_btid: NewBtid,
) -> DecodeResult<Vec<TargetClass>> {
    let (src, tpl) = btid_maps(source, template, layout, new_btid)?;
    targets_with(source, template, layout, &|b| src.get(&b).copied(), &|b| {
        tpl.get(&b).copied()
    })
}

/// The target classes at their final places for the index rebuild (workspace#503 `rebuild_indexes`, which groups
/// an index's owners by the BTID the class objects name): source BTIDs as they are, template BTIDs with the root
/// page id negated below -1, a value no real BTID has, so the two BTID spaces cannot merge two indexes. The
/// rebuild's `by_constraint` is what [`write_catalog`] takes; these BTIDs are never written.
pub fn index_build_classes(
    source: &SourceCatalog,
    template: &CatalogTemplate,
    layout: &CatalogLayout,
) -> DecodeResult<Vec<TargetClass>> {
    targets_with(source, template, layout, &Some, &|b| {
        Some(Btid {
            pageid: -2 - b.pageid,
            ..b
        })
    })
}

fn targets_with(
    source: &SourceCatalog,
    template: &CatalogTemplate,
    layout: &CatalogLayout,
    source_btid: &dyn Fn(Btid) -> Option<Btid>,
    template_btid: &dyn Fn(Btid) -> Option<Btid>,
) -> DecodeResult<Vec<TargetClass>> {
    let mut users: HashMap<Oid, Oid> = layout.users.clone();
    for t in template_rows(template, "_db_user")? {
        let name = t.values.get("name");
        if let Some(u) = source.same_slot["_db_user"]
            .iter()
            .find(|u| u.values.get("name") == name)
        {
            users.insert(t.oid, u.oid);
        }
    }
    let user = |o: Oid| *users.get(&o).unwrap_or(&o);
    let mut new_slot = |name: &[u8]| layout.new_slots.get(name).copied().unwrap_or(Oid::NULL);
    let mut new_heap =
        |_: Oid, name: &[u8]| layout.new_heaps.get(name).copied().unwrap_or(Hfid::NULL);
    let rep_dir = |oid: Oid| layout.records.get(&oid).map_or(Oid::NULL, |r| r.dir);
    target_classes(
        &source.classes,
        &source.plans,
        template,
        &mut Placement {
            new_slot: &mut new_slot,
            new_heap: &mut new_heap,
            rep_dir: &rep_dir,
            source_btid,
            template_btid,
            user: &user,
        },
    )
}

/// Writes the class objects and the catalog file (see the module header).
pub fn write_catalog(
    space: &SpaceManager,
    layout: &mut CatalogLayout,
    source: &SourceCatalog,
    template: &CatalogTemplate,
    new_btid: NewBtid,
) -> io::Result<()> {
    let vs = space.volumes();
    let targets = final_targets(source, template, layout, new_btid)
        .map_err(|e| invalid(format!("class objects: {e}")))?;
    let (_, boot) = BootDbParm::read(vs)?;
    let mut root = HeapWriter::open(space, boot.rootclass_hfid)?;
    for t in &targets {
        root.place(t.oid, &encode_develop(&t.class), false)?;
    }
    let stats = root.finish()?;
    layout.heap_stats.insert(boot.rootclass_hfid, stats);

    let mut items = Vec::with_capacity(3 * targets.len());
    for (t, &oid) in targets.iter().zip(&layout.class_order) {
        if t.oid != oid {
            return Err(invalid(format!(
                "class {:?} out of the planned catalog order",
                t.oid
            )));
        }
        let stats = layout
            .heap_stats
            .get(&t.class.heap)
            .copied()
            .unwrap_or_default();
        items.extend(
            class_catalog_items(t, layout.records[&oid], stats)
                .map_err(|e| invalid(format!("catalog records of {oid:?}: {e}")))?,
        );
    }
    write_items(space, layout, items)?;
    BootDbParm::write_ctid(vs, layout.catalog_vfid, layout.catalog_pages[0].pageid)
}

/// Puts the filled B-trees' statistics into the disk representations (each keeps its length) and writes the
/// catalog pages again. `stats(btid)` gives an index's statistics; an index it lacks keeps empty ones.
pub fn write_bt_stats(
    space: &SpaceManager,
    layout: &mut CatalogLayout,
    stats: &dyn Fn(Btid) -> Option<BtStats>,
) -> io::Result<()> {
    let mut items = layout.catalog_items.clone();
    for it in &mut items {
        if let CatalogItem::DiskRepr(r) = it {
            for a in r.fixed.iter_mut().chain(r.variable.iter_mut()) {
                for s in &mut a.bt_stats {
                    if let Some(new) = stats(s.btid) {
                        *s = BtStats {
                            btid: s.btid,
                            has_function: s.has_function,
                            ..new
                        };
                    }
                }
            }
        }
    }
    write_items(space, layout, items)
}

/// Builds the catalog pages from `items` on the planned pages and writes them; every record must land where the
/// plan put it.
fn write_items(
    space: &SpaceManager,
    layout: &mut CatalogLayout,
    items: Vec<CatalogItem>,
) -> io::Result<()> {
    let vs = space.volumes();
    let built = catalog_dir::build_pages(&items, vs.io_pagesize(), &layout.catalog_pages);
    let planned = catalog_dir::build_pages(&layout.catalog_items, vs.io_pagesize(), &[]);
    if built.at != planned.at || built.pages.len() != layout.catalog_pages.len() {
        return Err(invalid(
            "catalog records moved from their planned pages".into(),
        ));
    }
    catalog_dir::write_pages(vs, &built, &layout.catalog_pages)?;
    layout.catalog_items = items;
    Ok(())
}
