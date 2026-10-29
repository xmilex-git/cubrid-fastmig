//! The dry run of catalog regeneration: what it would free, rewrite, create and write, and where, without
//! touching the database. The real run (P1 integration, workspace#484) executes the same list.
//!
//! Input: the source volume set, its release line and the target's catalog template.
//! Output: [`DryRun`], printable as text, one section per step of decision workspace#482 10.
//! Source: decisions workspace#482 1, 2, 6, 7, 10; workspace#486 (deferred indexes); workspace#488
//! (recompile-plcsql.sql); R3 § 5 (class object and catalog rules); R5 § 3 (files freed before new ones).

use std::fmt;
use std::io;

use fastmig_ondisk::boot_parm::BootDbParm;
use fastmig_ondisk::class_object::ClassType;
use fastmig_ondisk::file_header::{FileDescriptor, FileHeader, FileType};
use fastmig_ondisk::volume_set::VolumeSet;
use fastmig_ondisk::{Oid, VersionLine, file_tracker};

use crate::classify::{ClassKind, classify};
use crate::source_classes::{SourceClass, read_classes};
use crate::system_classes::{SystemClassRole, target_role};
use crate::template::CatalogTemplate;

#[derive(Clone, Debug, Default)]
pub struct DryRun {
    /// `(step title, lines)` in execution order.
    pub sections: Vec<(String, Vec<String>)>,
    /// Non-empty means the migration refuses.
    pub refusals: Vec<String>,
}

impl fmt::Display for DryRun {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (title, lines) in &self.sections {
            writeln!(f, "== {title} ({})", lines.len())?;
            for l in lines {
                writeln!(f, "  {l}")?;
            }
        }
        writeln!(f, "== refusals ({})", self.refusals.len())?;
        for r in &self.refusals {
            writeln!(f, "  {r}")?;
        }
        Ok(())
    }
}

fn o(oid: Oid) -> String {
    format!("{}|{}|{}", oid.volid, oid.pageid, oid.slotid)
}

/// The class objects of a template, decoded (the template's records are develop records).
fn template_classes(t: &CatalogTemplate) -> io::Result<Vec<SourceClass>> {
    t.classes
        .iter()
        .map(|r| {
            fastmig_ondisk::class_object::decode(&r.bytes, VersionLine::Develop)
                .map(|class| SourceClass {
                    oid: r.oid,
                    class,
                    record: r.bytes.clone(),
                })
                .map_err(|e| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("template class {:?}: {e}", r.oid),
                    )
                })
        })
        .collect()
}

pub fn dry_run(
    vs: &VolumeSet,
    line: VersionLine,
    template: &CatalogTemplate,
) -> io::Result<DryRun> {
    let (_, boot) = BootDbParm::read(vs)?;
    let classes = read_classes(vs, line)?;
    let plans = classify(&classes);
    let tclasses = template_classes(template)?;
    let mut d = DryRun::default();

    // 1. classification
    let mut lines = Vec::new();
    for (c, p) in classes.iter().zip(&plans) {
        let what = match &p.kind {
            ClassKind::User => format!("user{}", if p.reencode { ", re-encoded" } else { "" }),
            ClassKind::System(r) => format!("system {r:?}"),
        };
        let part = if c.class.partitions.is_empty() {
            ""
        } else {
            ", partition info rewritten"
        };
        lines.push(format!(
            "{} {} repid {}: {what}{part}",
            o(c.oid),
            p.name,
            c.class.repid
        ));
        lines.extend(p.warnings.iter().map(|w| format!("warning: {w}")));
        d.refusals.extend(p.refusals.iter().cloned());
    }
    d.sections.push(("1 classify source classes".into(), lines));

    // 2. files freed before any new allocation
    let mut lines = Vec::new();
    let (items, _) = file_tracker::read(vs, boot.trk_vfid)?;
    let freed_heap = |oid: Oid| {
        plans.iter().any(|p| {
            p.oid == oid
                && matches!(
                    p.kind,
                    ClassKind::System(SystemClassRole::Reallocated | SystemClassRole::Dropped)
                )
        })
    };
    let name_of = |oid: Oid| {
        plans
            .iter()
            .find(|p| p.oid == oid)
            .map_or("?".to_string(), |p| p.name.clone())
    };
    for it in &items {
        let h = FileHeader::read(&vs.read_page(it.vfid.header_vpid())?);
        let why = match (h.file_type(), h.descriptor()) {
            (Some(FileType::Btree), FileDescriptor::ClassAttr { class_oid, .. }) => Some(format!(
                "B-tree of {} (every index is rebuilt)",
                name_of(class_oid)
            )),
            (Some(FileType::BtreeOverflowKey), FileDescriptor::BtreeOverflow { class_oid, .. }) => {
                Some(format!("B-tree overflow keys of {}", name_of(class_oid)))
            }
            (Some(FileType::Catalog), _) => Some("representation directory (regenerated)".into()),
            (
                Some(FileType::Heap | FileType::HeapReuseSlots),
                FileDescriptor::Heap { class_oid, .. },
            ) if freed_heap(class_oid) => Some(format!(
                "heap of {} (reallocated or dropped system table)",
                name_of(class_oid)
            )),
            (
                Some(FileType::MultipageObjectHeap),
                FileDescriptor::HeapOverflow { class_oid, .. },
            ) if freed_heap(class_oid) => Some(format!("overflow heap of {}", name_of(class_oid))),
            _ => None,
        };
        if let Some(why) = why {
            lines.push(format!(
                "free {}|{} {:?} {} pages: {why}",
                it.vfid.volid,
                it.vfid.fileid,
                h.file_type().unwrap(),
                h.n_page_user
            ));
        }
    }
    d.sections.push(("2 free old files".into(), lines));

    // 3. heaps that stay: header rewritten, bestspace pages added, records patched or re-encoded
    let mut lines = Vec::new();
    for (c, p) in classes.iter().zip(&plans) {
        let kept = matches!(
            p.kind,
            ClassKind::User | ClassKind::System(SystemClassRole::SameSlot { .. })
        );
        if kept && !c.class.heap.is_null() {
            let how = match &p.kind {
                ClassKind::System(SystemClassRole::SameSlot { develop_name }) => {
                    format!("rows rewritten in the develop layout of {develop_name} in their slots")
                }
                _ if p.reencode => format!("records re-encoded to repid {}", c.class.repid),
                _ => "records kept (patch)".into(),
            };
            lines.push(format!(
                "heap {}|{} hdr page {} of {}: develop header + bestspace after header (workspace#482 6); {how}",
                c.class.heap.vfid.volid, c.class.heap.vfid.fileid, c.class.heap.pageid, p.name
            ));
        }
    }
    d.sections.push(("3 heaps kept in place".into(), lines));

    // 5. class objects in the root heap, then the catalog
    let mut lines = Vec::new();
    let mut targets = 0usize;
    let mut target_atts = 0usize;
    for (c, p) in classes.iter().zip(&plans) {
        let action = match &p.kind {
            ClassKind::User => {
                targets += 1;
                target_atts += c.class.attributes.len()
                    + c.class.shared_attributes.len()
                    + c.class.class_attributes.len();
                "rewrite in place (new rep_dir and BTIDs, develop constraint sequences)".to_string()
            }
            ClassKind::System(SystemClassRole::SameSlot { .. } | SystemClassRole::Reallocated) => {
                let dn = develop_name_or_same(&p.kind, &p.name);
                let t = tclasses.iter().find(|t| t.class.name == dn.as_bytes());
                targets += 1;
                target_atts += t.map_or(0, |t| t.class.attributes.len());
                match (&p.kind, t) {
                    (_, None) => format!("NO TEMPLATE CLASS {dn}"),
                    (ClassKind::System(SystemClassRole::SameSlot { .. }), Some(t)) => format!(
                        "rewrite in place from template {dn} (template {}), heap kept",
                        o(t.oid)
                    ),
                    (_, Some(t)) => format!(
                        "rewrite in place from template {dn} (template {}), new heap",
                        o(t.oid)
                    ),
                }
            }
            ClassKind::System(SystemClassRole::Dropped | SystemClassRole::View) => {
                "delete slot".to_string()
            }
            ClassKind::System(_) => "?".into(),
        };
        lines.push(format!("{} {}: {action}", o(c.oid), p.name));
    }
    for t in &tclasses {
        let name = String::from_utf8_lossy(&t.class.name).into_owned();
        let role = target_role(&name, t.class.class_type == ClassType::Vclass);
        if matches!(role, Some(SystemClassRole::New | SystemClassRole::View)) {
            targets += 1;
            target_atts += t.class.attributes.len();
            let heap = if t.class.heap.is_null() {
                ""
            } else {
                ", new empty heap"
            };
            lines.push(format!("new slot {name} from template {}{heap}", o(t.oid)));
        }
    }
    lines.push(format!(
        "catalog: new catalog file, {targets} directory + class info + disk repr records; boot ctid updated"
    ));
    d.sections
        .push(("5 class objects and representation directory".into(), lines));

    // 5b. catalog rows
    let lines = vec![
        format!("_db_class: {targets} rows (one per target class)"),
        format!("_db_attribute: {target_atts} rows"),
        format!(
            "_db_data_type: {} rows, dual: {} row (template)",
            template.rows_of("_db_data_type").count(),
            template.rows_of("dual").count()
        ),
        "_db_collation, _db_charset: source rows re-encoded (workspace#482 3)".into(),
        "_db_auth: from the source grants plus the template's system grants".into(),
    ];
    d.sections.push(("5 catalog table rows".into(), lines));

    // 6. files written next to the target
    d.sections.push((
        "6 output files and the end".into(),
        vec![
            "<dest>/fastmig/update-statistics.sql: UPDATE STATISTICS ON ALL CLASSES; (workspace#482 7)".into(),
            "<dest>/fastmig/deferred-indexes.sql: CREATE INDEX for function and filter indexes (workspace#486)".into(),
            "<dest>/fastmig/recompile-plcsql.sql: PL/CSQL routines to recompile (workspace#488)".into(),
            "volume header paths, then _lgat last with sysmeta_version 1 (workspace#482 10)".into(),
        ],
    ));
    Ok(d)
}

fn develop_name_or_same(kind: &ClassKind, name: &str) -> String {
    match kind {
        ClassKind::System(SystemClassRole::SameSlot { develop_name }) => develop_name.to_string(),
        _ => name.to_string(),
    }
}
