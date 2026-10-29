//! The catalog writes of the same-version path (develop → develop): the catalog stays as the engine wrote it except
//! for what the index rebuild and the deferred indexes change. Heaps are patched, the deferred indexes' `_db_index`
//! and `_db_index_key` rows go (with their entries in `_db_class.indexes`), every class object names the rebuilt
//! BTIDs, and the representation directory is written again in a new catalog file.
//!
//! Input: the target's [`SpaceManager`] (a copy of a develop source), the source's vacuum horizon, the new BTID by
//! old BTID (one database: the old BTIDs are unique). Output: the target catalog; a [`CatalogLayout`] whose catalog
//! records [`crate::catalog_write::write_bt_stats`] fills once the B-trees are.
//! Source: 1st session decision 2026-10-29 (one catalog writer for both paths; the same-version path drops the
//! deferred indexes, workspace#486, and rewrites the BTIDs, ADR 0005); workspace#501 (same-version heap patch);
//! workspace#482 decision 10 (old files freed before new ones; rows removed before the index fill scans them).

use std::collections::HashMap;
use std::io;

use fastmig_ondisk::boot_parm::BootDbParm;
use fastmig_ondisk::catalog_dir::{self, CatalogItem};
use fastmig_ondisk::class_object::{class_properties, encode_develop};
use fastmig_ondisk::file_alloc::SpaceManager;
use fastmig_ondisk::file_header::{FileHeader, FileType};
use fastmig_ondisk::heap_rewrite::{
    HeapJob, HeapReport, HeapWriter, PageStats, VacuumHorizon, rewrite_heap,
};
use fastmig_ondisk::value::{DecodeResult, Value};
use fastmig_ondisk::{Btid, Oid, VersionLine, file_tracker};
use fastmig_progress::Meter;

use crate::catalog_layout::{CatalogLayout, CatalogRecords, class_catalog_items};
use crate::class_rows::{Row, read_rows};
use crate::develop_class_rewrite::rewrite_develop_class;
use crate::row_encode::encode_row;
use crate::source_classes::{SourceClass, read_classes};
use crate::system_table::SystemTable;
use crate::target_classes::{Origin, TargetClass};

const LINE: VersionLine = VersionLine::Develop;

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

/// The develop source's classes and its deferred indexes, read before the first write.
pub struct DevelopCatalog {
    pub classes: Vec<SourceClass>,
    /// Function and filter indexes, by (class OID, constraint name).
    pub deferred: Vec<(Oid, Vec<u8>)>,
}

impl DevelopCatalog {
    pub fn read(vs: &fastmig_ondisk::volume_set::VolumeSet) -> io::Result<Self> {
        let classes = read_classes(vs, LINE)?;
        let mut deferred = Vec::new();
        for c in &classes {
            if c.class.properties.is_empty() {
                continue;
            }
            let props = class_properties(&c.class.properties, LINE)
                .map_err(|e| invalid(format!("{:?} properties: {e}", c.oid)))?;
            for k in props.constraints {
                if k.has_filter || k.has_function {
                    deferred.push((c.oid, k.name));
                }
            }
        }
        Ok(DevelopCatalog { classes, deferred })
    }

    fn class(&self, name: &str) -> io::Result<&SourceClass> {
        self.classes
            .iter()
            .find(|c| c.class.name == name.as_bytes())
            .ok_or_else(|| invalid(format!("no class {name}")))
    }
}

/// Frees the catalog file; it is written anew by [`write_classes_and_catalog`].
pub fn free_catalog_file(space: &SpaceManager) -> io::Result<usize> {
    let vs = space.volumes();
    let (items, _) = file_tracker::read(vs, space.trk_vfid())?;
    let mut n = 0;
    for it in items {
        if FileHeader::read(&vs.read_page(it.vfid.header_vpid())?).file_type()
            == Some(FileType::Catalog)
        {
            space.destroy_file(it.vfid)?;
            n += 1;
        }
    }
    Ok(n)
}

/// Patches every class heap and the root heap (workspace#501: a heap without vacuum work stays byte for byte).
pub fn patch_heaps(
    space: &SpaceManager,
    cat: &DevelopCatalog,
    horizon: &VacuumHorizon,
    meter: Meter,
) -> io::Result<Vec<HeapReport>> {
    let (_, boot) = BootDbParm::read(space.volumes())?;
    let mut jobs = vec![(boot.rootclass_hfid, true)];
    jobs.extend(
        cat.classes
            .iter()
            .filter(|c| !c.class.heap.is_null())
            .map(|c| (c.class.heap, false)),
    );
    let mut out = Vec::with_capacity(jobs.len());
    for (hfid, root) in jobs {
        let r = rewrite_heap(
            space,
            horizon,
            &HeapJob {
                hfid,
                source_line: LINE,
                re_encode: None,
                root,
                pages_walked: Some(&|n| meter.add(n)),
            },
        )
        .map_err(|e| invalid(format!("heap {hfid:?}: {e}")))?;
        out.push(r);
    }
    Ok(out)
}

fn object(v: Option<&Value>) -> Option<Oid> {
    match v {
        Some(Value::Object(o)) => Some(*o),
        _ => None,
    }
}

fn text(v: Option<&Value>) -> Option<&[u8]> {
    match v {
        Some(Value::Varchar(s) | Value::Char(s)) => Some(s),
        _ => None,
    }
}

/// Removes the deferred indexes' `_db_index` and `_db_index_key` rows and their entries in `_db_class.indexes`.
/// Returns the rows deleted.
pub fn remove_deferred_rows(space: &SpaceManager, cat: &DevelopCatalog) -> io::Result<usize> {
    if cat.deferred.is_empty() {
        return Ok(0);
    }
    let vs = space.volumes();
    let (ct_class, ct_index, ct_key) = (
        cat.class(SystemTable::DbClass.name())?,
        cat.class(SystemTable::DbIndex.name())?,
        cat.class(SystemTable::DbIndexKey.name())?,
    );
    let class_rows = read_rows(vs, LINE, ct_class)?;
    let index_rows = read_rows(vs, LINE, ct_index)?;
    let key_rows = read_rows(vs, LINE, ct_key)?;
    // _db_class row by class object
    let row_of: HashMap<Oid, &Row> = class_rows
        .iter()
        .filter_map(|r| object(r.values.get("class_of")).map(|c| (c, r)))
        .collect();
    let mut dropped: Vec<Oid> = Vec::new();
    for (class, name) in &cat.deferred {
        let r = row_of
            .get(class)
            .ok_or_else(|| invalid(format!("no _db_class row for {class:?}")))?;
        let i = index_rows
            .iter()
            .find(|i| {
                object(i.values.get("class_of")) == Some(r.oid)
                    && text(i.values.get("index_name")) == Some(name)
            })
            .ok_or_else(|| {
                invalid(format!(
                    "no _db_index row for {class:?} {}",
                    String::from_utf8_lossy(name)
                ))
            })?;
        dropped.push(i.oid);
    }
    let keys: Vec<Oid> = key_rows
        .iter()
        .filter(|k| object(k.values.get("index_of")).is_some_and(|i| dropped.contains(&i)))
        .map(|k| k.oid)
        .collect();

    let mut deleted = 0;
    for (c, oids) in [(ct_key, &keys), (ct_index, &dropped)] {
        let mut w = HeapWriter::open(space, c.class.heap)?;
        for &o in oids {
            w.delete(o)?;
            deleted += 1;
        }
        w.finish()?;
    }
    let mut w = HeapWriter::open(space, ct_class.class.heap)?;
    for r in &class_rows {
        let Some(Value::Set(s)) = r.values.get("indexes") else {
            continue;
        };
        if !s
            .elements
            .iter()
            .any(|e| matches!(e.value, Value::Object(o) if dropped.contains(&o)))
        {
            continue;
        }
        let mut s = s.clone();
        s.elements
            .retain(|e| !matches!(e.value, Value::Object(o) if dropped.contains(&o)));
        let mut values = r.values.clone();
        values.insert("indexes".into(), Value::Set(s));
        let rec = encode_row(&ct_class.class, &Row { oid: r.oid, values })
            .map_err(|e| invalid(format!("_db_class {:?}: {e}", r.oid)))?;
        w.place(r.oid, &rec, true)?;
    }
    w.finish()?;
    Ok(deleted)
}

/// Writes every class object with the rebuilt BTIDs and without the deferred indexes, in a new catalog file whose
/// class info carries `heap_stats`; updates `ctid`. The catalog file must have been freed first.
pub fn write_classes_and_catalog(
    space: &SpaceManager,
    cat: &DevelopCatalog,
    new_btid: &dyn Fn(Btid) -> Option<Btid>,
    heap_stats: &HashMap<fastmig_ondisk::Hfid, PageStats>,
) -> io::Result<CatalogLayout> {
    let vs = space.volumes();
    let rewrite = |rep_dir: &dyn Fn(Oid) -> Oid| -> DecodeResult<Vec<TargetClass>> {
        cat.classes
            .iter()
            .map(|c| {
                Ok(TargetClass {
                    oid: c.oid,
                    class: rewrite_develop_class(&c.class, rep_dir(c.oid), new_btid)?,
                    origin: Origin::User,
                })
            })
            .collect()
    };
    let items = |targets: &[TargetClass], records: &dyn Fn(Oid) -> CatalogRecords| {
        targets
            .iter()
            .map(|t| {
                let stats = heap_stats.get(&t.class.heap).copied().unwrap_or_default();
                class_catalog_items(t, records(t.oid), stats)
            })
            .collect::<DecodeResult<Vec<_>>>()
            .map(|v| v.into_iter().flatten().collect::<Vec<CatalogItem>>())
    };
    let none = CatalogRecords {
        disk_repr: Oid::NULL,
        dir: Oid::NULL,
        cls_info: Oid::NULL,
    };
    let sizing_targets =
        rewrite(&|_| Oid::NULL).map_err(|e| invalid(format!("class objects: {e}")))?;
    let sizing =
        items(&sizing_targets, &|_| none).map_err(|e| invalid(format!("catalog records: {e}")))?;
    let sized = catalog_dir::build_pages(&sizing, vs.io_pagesize(), &[]);
    let (vfid, vpids) = catalog_dir::create_file(space, sized.pages.len())?;
    let at = |i: usize| {
        let (p, s) = sized.at[i];
        Oid::new(vpids[p].volid, vpids[p].pageid, s as i16)
    };
    let mut layout = CatalogLayout::empty();
    for (k, t) in sizing_targets.iter().enumerate() {
        layout.records.insert(
            t.oid,
            CatalogRecords {
                disk_repr: at(3 * k),
                dir: at(3 * k + 1),
                cls_info: at(3 * k + 2),
            },
        );
        layout.class_order.push(t.oid);
    }
    let targets =
        rewrite(&|o| layout.records[&o].dir).map_err(|e| invalid(format!("class objects: {e}")))?;
    let (_, boot) = BootDbParm::read(vs)?;
    let mut root = HeapWriter::open(space, boot.rootclass_hfid)?;
    for t in &targets {
        root.place(t.oid, &encode_develop(&t.class), false)?;
    }
    root.finish()?;
    let real = items(&targets, &|o| layout.records[&o])
        .map_err(|e| invalid(format!("catalog records: {e}")))?;
    let built = catalog_dir::build_pages(&real, vs.io_pagesize(), &vpids);
    if built.at != sized.at {
        return Err(invalid(
            "catalog records moved from their planned pages".into(),
        ));
    }
    catalog_dir::write_pages(vs, &built, &vpids)?;
    BootDbParm::write_ctid(vs, vfid, vpids[0].pageid)?;
    layout.catalog_vfid = vfid;
    layout.catalog_pages = vpids;
    layout.catalog_items = real;
    layout.heap_stats = heap_stats.clone();
    Ok(layout)
}
