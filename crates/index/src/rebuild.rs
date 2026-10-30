//! Rebuilds every plain index of the target: frees every B-tree and overflow key file the target holds, then creates
//! and fills one B-tree per index of the given classes (ADR 0005: the same-version path rebuilds them too).
//!
//! Input: the target's space manager (its line is the target line), the target classes (class objects in the target
//! line with their heaps; the heap pass already ran), the limits and an optional progress meter.
//! Output: [`RebuiltIndexes`] — the new BTID of every index by (class, constraint name) and by old BTID, and each
//! index's statistics.
//! Classes that share one BTID (the verifier's `IndexSpec` model) get one tree fed by all of them. Function and
//! filter indexes are not built (deferred indexes, workspace#486); constraints `SHARED:` with another keep no BTID.
//! Source: workspace#503; ADR 0005; R5 § 3 (files freed before new ones are allocated); engine `xbtree_load_index`.

use std::collections::BTreeMap;

use fastmig_ondisk::class_object::{ClassObject, Constraint, class_properties};
use fastmig_ondisk::file_alloc::SpaceManager;
use fastmig_ondisk::file_header::FileType;
use fastmig_ondisk::{Btid, Hfid, Oid, VersionLine, file_tracker};
use fastmig_progress::Meter;

use crate::btree_file::create_btree_file;
use crate::build_error::BuildError;
use crate::fill::{FilledIndex, Limits, fill_btrees};
use crate::index_def::{IndexDef, IndexOwner, first_key_attr, index_shape};

/// One class of the target with its heap, its class object in the target line's format.
pub struct TargetClass<'c> {
    pub oid: Oid,
    pub class: &'c ClassObject,
    pub hfid: Hfid,
}

pub struct RebuiltIndexes {
    /// New BTID by (class OID, constraint name), for every owner of every built index.
    pub by_constraint: BTreeMap<(Oid, Vec<u8>), Btid>,
    /// New BTID by the BTID the class objects named before (one source database; a template's BTIDs are another
    /// database's and may collide, so a catalog template looks up `by_constraint`).
    pub by_old_btid: BTreeMap<Btid, Btid>,
    /// In build order (old BTID ascending).
    pub filled: Vec<FilledIndex>,
    /// Indexes left out: function and filter indexes, as (class OID, constraint name).
    pub deferred: Vec<(Oid, Vec<u8>)>,
}

fn record_err(oid: Oid, what: String) -> BuildError {
    BuildError::Record { oid, what }
}

/// Frees every `FILE_BTREE` and `FILE_BTREE_OVERFLOW_KEY` file in the tracker; returns how many.
pub fn free_btree_files(space: &SpaceManager) -> std::io::Result<usize> {
    let (items, _) = file_tracker::read(space.volumes(), space.trk_vfid())?;
    let btree = [FileType::Btree as i16, FileType::BtreeOverflowKey as i16];
    let mut n = 0;
    for it in items.iter().filter(|it| btree.contains(&it.file_type_raw)) {
        space.destroy_file(it.vfid)?;
        n += 1;
    }
    Ok(n)
}

/// A function or filter index left out: (class OID, constraint name).
pub type DeferredIndex = (Oid, Vec<u8>);

/// Each class's constraints (class objects in `line`), parsed once (empty for a class with no properties).
pub fn class_constraints(
    classes: &[TargetClass],
    line: VersionLine,
) -> Result<Vec<Vec<Constraint>>, BuildError> {
    classes
        .iter()
        .map(|c| {
            if c.class.properties.is_empty() {
                return Ok(Vec::new());
            }
            Ok(class_properties(&c.class.properties, line)
                .map_err(|e| record_err(c.oid, format!("class properties: {e}")))?
                .constraints)
        })
        .collect()
}

/// The plain indexes of `classes` (`cons` from [`class_constraints`]): one definition per BTID the class objects
/// name, its owners the classes that share it, `btid` still that old BTID; function and filter indexes come back as
/// deferred (class OID, constraint name). Shapes and key domains are in the `target` line. Reads nothing from the
/// volumes: a plan can call it on a source.
pub fn plain_index_defs<'c>(
    classes: &[TargetClass<'c>],
    cons: &'c [Vec<Constraint>],
    io_pagesize: usize,
    db_charset: u8,
    target: VersionLine,
) -> Result<(Vec<IndexDef<'c>>, Vec<DeferredIndex>), BuildError> {
    // old BTID -> its owners (class index, constraint)
    let mut by_btid: BTreeMap<Btid, Vec<(usize, &Constraint)>> = BTreeMap::new();
    let mut deferred = Vec::new();
    for (i, cs) in cons.iter().enumerate() {
        for k in cs {
            let Some(b) = k.btid else { continue };
            if k.has_function || k.has_filter {
                deferred.push((classes[i].oid, k.name.clone()));
                continue;
            }
            let owners = by_btid.entry(b).or_default();
            if !owners.iter().any(|(j, _)| *j == i) {
                owners.push((i, k));
            }
        }
    }
    let mut defs = Vec::with_capacity(by_btid.len());
    for (old, owners) in by_btid {
        let owners: Vec<IndexOwner> = owners
            .iter()
            .map(|&(i, k)| IndexOwner {
                class_oid: classes[i].oid,
                class: classes[i].class,
                hfid: classes[i].hfid,
                constraint: k,
            })
            .collect();
        let first = &owners[0];
        let (shape, key_domain) =
            index_shape(first, io_pagesize, db_charset, target).map_err(|e| {
                record_err(
                    first.class_oid,
                    format!(
                        "index {}: {e}",
                        String::from_utf8_lossy(&first.constraint.name)
                    ),
                )
            })?;
        defs.push(IndexDef {
            btid: old,
            owners,
            shape,
            key_domain,
        });
    }
    Ok((defs, deferred))
}

pub fn rebuild_indexes(
    space: &SpaceManager,
    classes: &[TargetClass],
    lim: &Limits,
    progress: Option<Meter>,
) -> Result<RebuiltIndexes, BuildError> {
    let target = space.line();
    let cons = class_constraints(classes, target)?;
    let io_pagesize = space.volumes().io_pagesize();
    let db_charset = space.volumes().volume(0)?.header.db_charset as u8;
    let (mut defs, deferred) = plain_index_defs(classes, &cons, io_pagesize, db_charset, target)?;

    free_btree_files(space)?;
    let mut by_constraint = BTreeMap::new();
    let mut by_old_btid = BTreeMap::new();
    for d in &mut defs {
        let btid = create_btree_file(space, &d.shape, first_key_attr(d.owners[0].constraint))?;
        for o in &d.owners {
            by_constraint.insert((o.class_oid, o.constraint.name.clone()), btid);
        }
        by_old_btid.insert(d.btid, btid);
        d.btid = btid;
    }
    let filled = fill_btrees(space, &defs, lim, progress)?;
    Ok(RebuiltIndexes {
        by_constraint,
        by_old_btid,
        filled,
        deferred,
    })
}
