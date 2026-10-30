//! The catalog-regenerating part of `migrate` (10.2..11.4 → develop, 10.2..11.3 → 11.4: the target line is
//! `SpaceManager::line`) between the volume copy and the new log: old catalog files freed, user heaps
//! patched or re-encoded, the catalog regenerated (system rows, class objects, catalog file, `ctid`).
//!
//! Input: the [`SpaceManager`] over the target volumes, the source's log header, the worker limit and the
//! phase meters; the caller laps a phase after each of [`prepare`] (page treatment, catalog placement and system rows),
//! [`build_indexes`] (B-tree files, workspace#503) and [`finish`] (class objects, catalog file, BT_STATS); the
//! same-version path has the same three steps. Output: the target volumes in
//! the target line's catalog format; a [`CatalogOutcome`] for report.json.
//! Source: workspace#482 decision 10 (order: free, page treatment, index build, class objects and catalog);
//! workspace#504 (catalog writer: `plan_catalog` before the BTIDs exist, `write_catalog` after, `write_bt_stats` once
//! the B-trees are filled).

use fastmig_catalog::catalog_layout::{CatalogLayout, plan_catalog};
use fastmig_catalog::catalog_write::{index_build_classes, write_bt_stats, write_catalog};
use fastmig_catalog::free_old_files::free_old_files;
use fastmig_catalog::page_treatment::treat_user_heaps;
use fastmig_catalog::same_version_catalog::{
    SameVersionCatalog, free_catalog_file, patch_heaps, remove_deferred_rows,
    write_classes_and_catalog,
};
use fastmig_catalog::source_catalog::SourceCatalog;
use fastmig_catalog::template::CatalogTemplate;
use fastmig_catalog::template_store::builtin;
use fastmig_index::memory_limit::MemoryLimit;
use fastmig_index::rebuild::{
    RebuiltIndexes, TargetClass as IndexClass, free_btree_files, rebuild_indexes,
};
use fastmig_index::spill_dir::SpillDir;
use fastmig_index::worker_limit::WorkerLimit;
use fastmig_ondisk::boot_parm::BootDbParm;
use fastmig_ondisk::catalog_dir::{BtStats, PKEYS_NUM};
use fastmig_ondisk::file_alloc::SpaceManager;
use fastmig_ondisk::heap_rewrite::{HeapReport, VacuumHorizon};
use fastmig_ondisk::log_header::LogHeader;
use fastmig_ondisk::{Btid, Oid, SourceVersion, VersionLine};
use fastmig_ondisk::{tde_metadata, vacuum_data};
use fastmig_progress::Meter;
use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::Path;

use crate::limits::Limits;
use crate::local_datetime::now_datetime;

/// What the catalog part did, for report.json.
pub struct CatalogOutcome {
    pub freed_files: usize,
    pub user_heaps: usize,
    /// User heaps whose statistics were measured (every chain page read).
    pub measured_heaps: usize,
    /// Pages the page treatment appended to the heaps (workspace#526: against `appended_pages_bound`).
    pub appended_pages: u64,
    /// User heap pages rewritten, and of those the isolated pages the parallel first pass wrote (workspace#536).
    pub rewritten_pages: u64,
    pub isolated_pages: u64,
    pub rows_written: BTreeMap<&'static str, usize>,
    pub new_slots: usize,
    pub new_heaps: usize,
    pub deleted_classes: usize,
    pub catalog_pages: usize,
    pub indexes_built: usize,
}

impl CatalogOutcome {
    pub fn json(&self) -> String {
        let rows: Vec<String> = self
            .rows_written
            .iter()
            .map(|(t, n)| format!("\"{t}\": {n}"))
            .collect();
        format!(
            "{{\"freed_files\": {}, \"user_heaps\": {}, \"measured_heaps\": {}, \"appended_pages\": {}, \"rewritten_pages\": {}, \"isolated_pages\": {}, \"new_slots\": {}, \"new_heaps\": {}, \"deleted_classes\": {}, \"catalog_pages\": {}, \"indexes_built\": {}, \"rows_written\": {{{}}}}}",
            self.freed_files,
            self.user_heaps,
            self.measured_heaps,
            self.appended_pages,
            self.rewritten_pages,
            self.isolated_pages,
            self.new_slots,
            self.new_heaps,
            self.deleted_classes,
            self.catalog_pages,
            self.indexes_built,
            rows.join(", ")
        )
    }
}

/// The state between [`prepare`] and [`finish`]: the source catalog read before the first write and where the new
/// catalog goes.
pub struct Prepared {
    source: SourceCatalog,
    template: CatalogTemplate,
    layout: CatalogLayout,
    freed_files: usize,
    reports: Vec<HeapReport>,
}

impl Prepared {
    /// Classes whose heap statistics were measured (every chain page read), for verify.
    pub fn measured(&self) -> impl Iterator<Item = Oid> + '_ {
        self.reports
            .iter()
            .filter(|r| r.measured)
            .map(|r| r.class_oid)
    }
}

/// Frees the old catalog files, treats the user heaps (page-treatment phase), places the catalog and writes its rows.
/// `tde_keys`: where a source without TDE metadata (10.2) gets its new `_keys` file.
pub fn prepare(
    space: &SpaceManager,
    src_version: SourceVersion,
    tde_keys: &Path,
    src_log: &LogHeader,
    src_lob_base: Option<&Path>,
    worker_limit: usize,
    meter: Meter,
) -> io::Result<Prepared> {
    let vols = space.volumes();
    let (_, boot) = BootDbParm::read(vols)?;
    // everything the regeneration reads from the source catalog, before the first write
    let target = space.line();
    let source = SourceCatalog::read(vols, src_version, target)?;
    let template = builtin(target, source.charset).ok_or_else(|| {
        io::Error::other(format!(
            "no catalog template for charset {}",
            source.charset
        ))
    })?;
    let horizon = VacuumHorizon::read(vols, src_log, boot.vacuum_data_vfid)?;
    // its entries name source log blocks the target lacks; read into the horizon above, then emptied (workspace#551)
    vacuum_data::empty(space, boot.vacuum_data_vfid)?;
    // every old file goes before the first new allocation (workspace#482 10.2), B-trees included (workspace#503)
    let freed_files = free_old_files(space, &source)?.len() + free_btree_files(space)?;
    // 10.2 has no TDE metadata; develop needs it to load its TDE module and run vacuum. Its heap must lie in volume 0,
    // which the B-trees just freed leave room in (workspace#515)
    if src_version == SourceVersion::V10_2 {
        // time(NULL), as the engine stamps a new master key
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        tde_metadata::create(space, tde_keys, now)?;
    }
    let reports = treat_user_heaps(space, &source, &horizon, src_lob_base, worker_limit, meter)?;
    let stats: HashMap<_, _> = reports.iter().map(|r| (r.hfid, r.stats)).collect();
    let layout = plan_catalog(space, &source, &template, &horizon, &now_datetime(), stats)?;
    Ok(Prepared {
        source,
        template,
        layout,
        freed_files,
        reports,
    })
}

/// Rebuilds every plain index of `owners` under the run's limits (index-build phase).
fn rebuild(
    space: &SpaceManager,
    owners: &[IndexClass],
    limits: &Limits,
    worker_limit: usize,
    index_meter: Meter,
) -> io::Result<RebuiltIndexes> {
    let workers = WorkerLimit::new(worker_limit);
    let memory = MemoryLimit::new(usize::try_from(limits.memory_limit_bytes).unwrap_or(usize::MAX));
    let spill = match &limits.arena {
        Some(a) => SpillDir::in_arena(a.clone()),
        None => SpillDir::create(&limits.spill_dir)?,
    };
    rebuild_indexes(
        space,
        owners,
        &fastmig_index::Limits {
            workers: &workers,
            memory: &memory,
            spill: &spill,
            temp_disk: limits.temp_disk,
        },
        Some(index_meter),
    )
    .map_err(|e| io::Error::other(format!("index rebuild: {e}")))
}

/// Rebuilds every plain index of the regenerated classes (index-build phase).
pub fn build_indexes(
    space: &SpaceManager,
    p: &Prepared,
    limits: &Limits,
    worker_limit: usize,
    index_meter: Meter,
) -> io::Result<RebuiltIndexes> {
    let classes = index_build_classes(&p.source, &p.template, &p.layout)
        .map_err(|e| io::Error::other(format!("class objects for the index rebuild: {e}")))?;
    let owners: Vec<IndexClass> = classes
        .iter()
        .filter(|t| !t.class.heap.is_null())
        .map(|t| IndexClass {
            oid: t.oid,
            class: &t.class,
            hfid: t.class.heap,
        })
        .collect();
    let rebuilt = rebuild(space, &owners, limits, worker_limit, index_meter)?;
    // every BTID written comes from the rebuild, never a grouping tag of `index_build_classes`
    if let Some(((c, n), b)) = rebuilt.by_constraint.iter().find(|(_, b)| b.pageid < 0) {
        return Err(io::Error::other(format!(
            "index rebuild gave {c:?} {} the BTID {b:?}",
            String::from_utf8_lossy(n)
        )));
    }
    Ok(rebuilt)
}

/// Writes the class objects with the new BTIDs, the catalog file and the B-tree statistics (catalog-regeneration
/// phase).
pub fn finish(
    space: &SpaceManager,
    mut p: Prepared,
    rebuilt: RebuiltIndexes,
) -> io::Result<CatalogOutcome> {
    write_catalog(
        space,
        &mut p.layout,
        &p.source,
        &p.template,
        &|class, name| rebuilt.by_constraint.get(&(class, name.to_vec())).copied(),
    )?;
    let stats = bt_stats_by_btid(&rebuilt);
    write_bt_stats(space, &mut p.layout, &|b| stats.get(&b).cloned())?;
    Ok(CatalogOutcome {
        freed_files: p.freed_files,
        user_heaps: p.reports.len(),
        measured_heaps: p.reports.iter().filter(|r| r.measured).count(),
        appended_pages: p.reports.iter().map(|r| r.appended_pages).sum(),
        rewritten_pages: p.reports.iter().map(|r| r.pages_rewritten).sum(),
        isolated_pages: p.reports.iter().map(|r| r.isolated_pages).sum(),
        rows_written: p.layout.rows_written.clone(),
        new_slots: p.layout.new_slots.len(),
        new_heaps: p.layout.new_heaps.len(),
        deleted_classes: p.layout.deleted_classes.len(),
        catalog_pages: p.layout.catalog_pages.len(),
        indexes_built: rebuilt.filled.len(),
    })
}

/// The catalog-row form of every filled index's statistics, by its new BTID.
fn bt_stats_by_btid(rebuilt: &RebuiltIndexes) -> HashMap<Btid, BtStats> {
    rebuilt
        .filled
        .iter()
        .map(|f| (f.btid, bt_stats(f.btid, &f.stats)))
        .collect()
}

/// The disk-representation form of a filled index's statistics (`has_function` is set by the catalog writer).
// a partition root keeps its own (empty) tree's statistics, not the partitions' sum UPDATE STATISTICS writes; the
// update-statistics.sql of workspace#482 decision 7 replaces them (workspace#503)
fn bt_stats(btid: Btid, s: &fastmig_index::bt_stats::BtStats) -> BtStats {
    let mut pkeys = [0i64; PKEYS_NUM];
    for (d, v) in pkeys.iter_mut().zip(&s.pkeys) {
        *d = *v as i64;
    }
    BtStats {
        btid,
        leafs: s.leafs as i32,
        pages: s.pages as i32,
        height: s.height as i32,
        has_function: 0,
        keys: s.keys as i64,
        pkeys,
    }
}

/// The state between [`same_version_prepare`] and [`same_version_finish`]: the target's catalog read before
/// the first write, and what the page treatment did.
pub struct SameVersionPrepared {
    cat: SameVersionCatalog,
    freed_files: usize,
    reports: Vec<HeapReport>,
    deleted_rows: usize,
}

impl SameVersionPrepared {
    /// Classes whose heap statistics were measured (every chain page read), for verify.
    pub fn measured(&self) -> impl Iterator<Item = Oid> + '_ {
        self.reports
            .iter()
            .filter(|r| r.measured && !r.class_oid.is_null())
            .map(|r| r.class_oid)
    }
}

/// The same-version (develop → develop, 11.4 → 11.4; `line` is both) first half: the catalog file and every B-tree freed, heaps patched
/// (page-treatment phase), the deferred indexes' catalog rows removed.
pub fn same_version_prepare(
    space: &SpaceManager,
    line: VersionLine,
    src_log: &LogHeader,
    treatment_meter: Meter,
) -> io::Result<SameVersionPrepared> {
    let vols = space.volumes();
    let (_, boot) = BootDbParm::read(vols)?;
    let cat = SameVersionCatalog::read(vols, line)?;
    let horizon = VacuumHorizon::read(vols, src_log, boot.vacuum_data_vfid)?;
    // its entries name source log blocks the target lacks; read into the horizon above, then emptied (workspace#551)
    vacuum_data::empty(space, boot.vacuum_data_vfid)?;
    // every old file goes before the first new allocation (workspace#482 10.2)
    let freed_files = free_catalog_file(space)? + free_btree_files(space)?;
    let reports = patch_heaps(space, &cat, &horizon, treatment_meter)?;
    let deleted_rows = remove_deferred_rows(space, &cat)?;
    Ok(SameVersionPrepared {
        cat,
        freed_files,
        reports,
        deleted_rows,
    })
}

/// Rebuilds every plain index of the target's classes (index-build phase).
pub fn same_version_build_indexes(
    space: &SpaceManager,
    p: &SameVersionPrepared,
    limits: &Limits,
    worker_limit: usize,
    index_meter: Meter,
) -> io::Result<RebuiltIndexes> {
    let owners: Vec<IndexClass> = p
        .cat
        .classes
        .iter()
        .filter(|c| !c.class.heap.is_null())
        .map(|c| IndexClass {
            oid: c.oid,
            class: &c.class,
            hfid: c.class.heap,
        })
        .collect();
    rebuild(space, &owners, limits, worker_limit, index_meter)
}

/// The same-version second half: class objects and catalog file written with the new BTIDs, then the B-tree
/// statistics (catalog-regeneration phase).
pub fn same_version_finish(
    space: &SpaceManager,
    p: SameVersionPrepared,
    rebuilt: RebuiltIndexes,
) -> io::Result<CatalogOutcome> {
    // after the patch every heap's header holds its statistics: measured, or the source's kept as is
    let heap_stats: HashMap<_, _> = p.reports.iter().map(|r| (r.hfid, r.stats)).collect();
    let mut layout = write_classes_and_catalog(
        space,
        &p.cat,
        &|b| rebuilt.by_old_btid.get(&b).copied(),
        &heap_stats,
    )?;
    let stats = bt_stats_by_btid(&rebuilt);
    write_bt_stats(space, &mut layout, &|b| stats.get(&b).cloned())?;
    let mut rows_written = BTreeMap::new();
    rows_written.insert("deleted deferred-index rows", p.deleted_rows);
    Ok(CatalogOutcome {
        freed_files: p.freed_files,
        user_heaps: p.reports.len(),
        measured_heaps: p.reports.iter().filter(|r| r.measured).count(),
        appended_pages: p.reports.iter().map(|r| r.appended_pages).sum(),
        rewritten_pages: p.reports.iter().map(|r| r.pages_rewritten).sum(),
        isolated_pages: p.reports.iter().map(|r| r.isolated_pages).sum(),
        rows_written,
        new_slots: 0,
        new_heaps: 0,
        deleted_classes: 0,
        catalog_pages: layout.catalog_pages.len(),
        indexes_built: rebuilt.filled.len(),
    })
}
