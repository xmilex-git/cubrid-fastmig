//! Check 3 (catalog): the references a class object holds, and the files nothing references.
//!
//! Input: the database, its tracked files (check 4), the live OID set, every class's parsed constraints, and the
//! OBJECT references the heap passes collected (for `_db_class.class_of`). Output: check 3 defects:
//! - `db-class-row`: class objects and `_db_class.class_of` values are 1:1;
//! - `rep-dir`: the representation directory record exists and lists the class-info item and the current repid;
//! - `heap-file`, `btree-file`: the class's HFID / each index's BTID names a tracked file of the right type whose
//!   descriptor names the class (and a B-tree root whose `topclass_oid` does);
//! - `orphan-heap`, `orphan-btree`: tracked heap / B-tree files no class references (the root heap excepted);
//!   overflow heap / overflow key files whose owner is not a referenced heap / B-tree;
//! - `owner`, `trigger`: the owner is a live `db_user` row, each trigger a live `db_trigger` row;
//! - `fk-ref`: a foreign key's referenced class exists and its referenced index is that class's;
//! - `partition-link`: each sub class lists the class among its super classes and the reverse.
//!
//! Synonym and grant targets, SP owners and every other catalog OBJECT value go through the OBJECT check
//! (`object_refs::check_refs`); a synonym names its target by name, not OID, and is not checked here.
//! Source: G3 workspace#483 Q7; R3 § 2 (class object fields), R4 (catalog tables); engine `boot_sr.c` root heap.

use std::collections::{BTreeSet, HashMap, HashSet};

use fastmig_catalog::system_table::SystemTable;
use fastmig_ondisk::btree::root_header::RootHeader;
use fastmig_ondisk::catalog_dir::ReprDir;
use fastmig_ondisk::class_object::{ClassType, Constraint};
use fastmig_ondisk::file_header::{FileDescriptor, FileType};
use fastmig_ondisk::page;
use fastmig_ondisk::spage::SlottedPage;
use fastmig_ondisk::{Btid, Oid, Vfid};

use crate::database::Database;
use crate::defect::{Check, Defect, OidText};
use crate::live_row_map::LiveRowMap;
use crate::object_refs::ObjectRef;
use crate::sector_owner::Files;

pub struct CatalogInput<'a> {
    pub db: &'a Database,
    pub files: &'a Files,
    pub live: &'a LiveRowMap<'a>,
    /// Per class (index into `db.classes`): its constraints, or the decode error.
    pub constraints: &'a [Result<Vec<Constraint>, String>],
    pub refs: &'a [ObjectRef],
}

fn vfid_text(v: Vfid) -> String {
    format!("file {}|{}", v.volid, v.fileid)
}

pub fn check_catalog(c: &CatalogInput) -> Vec<Defect> {
    let db = c.db;
    let mut out = Vec::new();
    let user = db.system_table(SystemTable::DbUser).map(|s| s.oid);
    let trigger = db.system_table(SystemTable::DbTrigger).map(|s| s.oid);
    // heaps of no class: the root heap, the boot parameter heap and the TDE key heap (BOOT_DB_PARM)
    let mut heaps: HashSet<Vfid> = [
        db.boot.rootclass_hfid,
        db.boot.hfid,
        db.boot.tde_keyinfo_hfid,
    ]
    .iter()
    .filter(|h| !h.is_null())
    .map(|h| h.vfid)
    .collect();
    let mut btrees: HashSet<Vfid> = HashSet::new();
    for (i, sc) in db.classes.iter().enumerate() {
        let cls = &sc.class;
        let name = String::from_utf8_lossy(&cls.name).into_owned();
        let d = |code| {
            Defect::new(Check::Catalog, code)
                .class(name.clone())
                .object(OidText(sc.oid))
        };
        if !cls.rep_dir.is_null() {
            match read_rep_dir(db, cls.rep_dir) {
                Ok(rd) => {
                    let has_rep = rd.items.iter().any(|it| it.repr_id as i32 == cls.repid);
                    if rd.class_info().is_none() || !has_rep {
                        out.push(
                            d("rep-dir")
                                .expected(format!("class info item and repid {}", cls.repid))
                                .actual(format!("{:?}", rd.items)),
                        );
                    }
                }
                Err(e) => out.push(d("rep-dir").actual(e)),
            }
        } else if cls.class_type == ClassType::Class {
            out.push(
                d("rep-dir")
                    .expected("a representation directory")
                    .actual("NULL"),
            );
        }
        if !cls.heap.is_null() {
            heaps.insert(cls.heap.vfid);
            match c.files.get(&cls.heap.vfid) {
                Some((_, ft))
                    if matches!(
                        ft.header.file_type(),
                        Some(FileType::Heap | FileType::HeapReuseSlots)
                    ) && matches!(ft.header.descriptor(), FileDescriptor::Heap { class_oid, hfid }
                            if class_oid == sc.oid && hfid == cls.heap) => {}
                Some((_, ft)) => out.push(
                    d("heap-file")
                        .expected(format!("heap file of {}", OidText(sc.oid)))
                        .actual(format!(
                            "{:?} {:?}",
                            ft.header.file_type(),
                            ft.header.descriptor()
                        )),
                ),
                None => out.push(
                    d("heap-file").actual(format!("{} not tracked", vfid_text(cls.heap.vfid))),
                ),
            }
        }
        if !cls.owner.is_null() && !(user.is_some() && c.live.class_of(cls.owner) == user) {
            out.push(
                d("owner")
                    .expected("a live db_user row")
                    .actual(OidText(cls.owner)),
            );
        }
        for t in &cls.triggers {
            if !(trigger.is_some() && c.live.class_of(*t) == trigger) {
                out.push(
                    d("trigger")
                        .expected("a live db_trigger row")
                        .actual(OidText(*t)),
                );
            }
        }
        for sub in &cls.sub_classes {
            let back = db
                .by_oid
                .get(sub)
                .is_some_and(|&j| db.classes[j].class.super_classes.contains(&sc.oid));
            if !back {
                out.push(
                    d("partition-link")
                        .expected(format!("{} lists the class as super", OidText(*sub))),
                );
            }
        }
        for sup in &cls.super_classes {
            let back = db
                .by_oid
                .get(sup)
                .is_some_and(|&j| db.classes[j].class.sub_classes.contains(&sc.oid));
            if !back {
                out.push(
                    d("partition-link")
                        .expected(format!("{} lists the class as sub", OidText(*sup))),
                );
            }
        }
        let cons = match &c.constraints[i] {
            Ok(v) => v,
            Err(e) => {
                out.push(d("constraints").actual(e));
                continue;
            }
        };
        for k in cons {
            let iname = String::from_utf8_lossy(&k.name).into_owned();
            if let Some(b) = k.btid {
                btrees.insert(b.vfid);
                if let Err(e) = check_btid(db, c.files, b, sc.oid) {
                    out.push(d("btree-file").index(iname.clone()).actual(e));
                }
            }
            if let Some(fk) = &k.foreign_key {
                let ok = db.by_oid.get(&fk.ref_class).is_some_and(|&j| {
                    match (&c.constraints[j], fk.ref_pk_btid) {
                        (Ok(rc), Some(b)) => rc.iter().any(|x| x.btid == Some(b)),
                        _ => false,
                    }
                });
                if !ok {
                    out.push(
                        d("fk-ref")
                            .index(iname)
                            .expected("referenced class and its index")
                            .actual(format!("{} {:?}", OidText(fk.ref_class), fk.ref_pk_btid)),
                    );
                }
            }
        }
    }
    check_db_class(c, &mut out);
    for (vfid, (_, ft)) in c.files {
        let orphan = match (ft.header.file_type(), ft.header.descriptor()) {
            (Some(FileType::Heap | FileType::HeapReuseSlots), _) => {
                (!heaps.contains(vfid)).then_some("orphan-heap")
            }
            (Some(FileType::Btree), _) => (!btrees.contains(vfid)).then_some("orphan-btree"),
            (_, FileDescriptor::HeapOverflow { hfid, .. }) => {
                (!heaps.contains(&hfid.vfid)).then_some("orphan-heap")
            }
            (_, FileDescriptor::BtreeOverflow { btid, .. }) => {
                (!btrees.contains(&btid.vfid)).then_some("orphan-btree")
            }
            _ => None,
        };
        if let Some(code) = orphan {
            out.push(
                Defect::new(Check::Catalog, code)
                    .object(vfid_text(*vfid))
                    .actual(format!("{:?}", ft.header.descriptor())),
            );
        }
    }
    out
}

fn read_rep_dir(db: &Database, at: Oid) -> Result<ReprDir, String> {
    let p = db.vols.read_page(at.vpid()).map_err(|e| e.to_string())?;
    let s = SlottedPage::new(page::db_area(&p)).ok_or("catalog page is not slotted")?;
    let (_, rec) = s
        .record(at.slotid.max(0) as usize)
        .ok_or("no record in the slot")?;
    ReprDir::decode(rec).map_err(|e| e.to_string())
}

fn check_btid(db: &Database, files: &Files, b: Btid, class: Oid) -> Result<(), String> {
    let (_, ft) = files
        .get(&b.vfid)
        .ok_or_else(|| format!("{} not tracked", vfid_text(b.vfid)))?;
    if ft.header.file_type() != Some(FileType::Btree) {
        return Err(format!(
            "{} is {:?}",
            vfid_text(b.vfid),
            ft.header.file_type()
        ));
    }
    let root = db.vols.read_page(b.vpid()).map_err(|e| e.to_string())?;
    let rec = SlottedPage::new(page::db_area(&root))
        .and_then(|s| s.record(0).map(|r| r.1.to_vec()))
        .ok_or("no root header")?;
    let rh = RootHeader::read_layout(&rec, db.version.header_layout()).map_err(|e| e.what)?;
    let desc_class = match ft.header.descriptor() {
        FileDescriptor::ClassAttr { class_oid, .. } => class_oid,
        other => return Err(format!("descriptor {other:?}")),
    };
    if desc_class != class || rh.topclass_oid != class {
        return Err(format!(
            "descriptor class {} root topclass {}, class {}",
            OidText(desc_class),
            OidText(rh.topclass_oid),
            OidText(class)
        ));
    }
    Ok(())
}

/// `_db_class.class_of` values = the class OIDs, both ways.
fn check_db_class(c: &CatalogInput, out: &mut Vec<Defect>) {
    let db = c.db;
    let Some(dbc) = db.system_table(SystemTable::DbClass) else {
        out.push(Defect::new(Check::Catalog, "db-class-row").expected("a _db_class class"));
        return;
    };
    let Some(attr) = dbc
        .class
        .attributes
        .iter()
        .find(|a| a.name == b"class_of")
        .map(|a| a.id)
    else {
        out.push(Defect::new(Check::Catalog, "db-class-row").expected("_db_class.class_of"));
        return;
    };
    let mut rows: HashMap<Oid, usize> = HashMap::new();
    for r in c
        .refs
        .iter()
        .filter(|r| r.attr == attr && c.live.class_of(r.from) == Some(dbc.oid))
    {
        *rows.entry(r.to).or_default() += 1;
    }
    let classes: BTreeSet<Oid> = db.classes.iter().map(|s| s.oid).collect();
    for oid in &classes {
        let n = rows.get(oid).copied().unwrap_or(0);
        if n != 1 {
            out.push(
                Defect::new(Check::Catalog, "db-class-row")
                    .class(db.class_name(*oid))
                    .object(OidText(*oid))
                    .expected("1 _db_class row")
                    .actual(n),
            );
        }
    }
    for oid in rows.keys().filter(|o| !classes.contains(o)) {
        out.push(
            Defect::new(Check::Catalog, "db-class-row")
                .object(OidText(*oid))
                .expected("a class object")
                .actual("_db_class row of no class"),
        );
    }
}
