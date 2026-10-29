//! `cubrid-fastmig migrate`: moves a cleanly shut down database to a new directory and registers it; with `--verify`
//! it verifies the target first.
//!
//! Input: the source DB name (found in the source `databases.txt`), the target directory and options.
//! Output: the target volumes, a new `_lgat`, `_vinf`, `_keys`, Java SP directories, referenced LOB files, and in
//! `<dest>/fastmig/`: `plan.json`, `report.json`, `verify.jsonl` (with `--verify`), the SQL files for the user
//! (`deferred-indexes.sql`, `recompile-plcsql.sql`, `update-statistics.sql`), `lob-manifest`, `lob-outside-base`,
//! `measured-classes` and `source-fingerprint` (read by finalize); then the target's `databases.txt` line. Progress
//! (the phases in `PHASES`) goes to stderr. The source is only read; its active log is read-locked for the whole run.
//! Order: the plan refuses before anything is written; the new log comes last of the writes; with `--verify` the
//! verifier runs on the durable target and only a target with 0 migration defects is registered and loses its
//! incomplete marker; without it every finished run is registered (ADR 0015) and `verify.jsonl` is not written.
//! Between the copy and the new log: page treatment, index rebuild and catalog regeneration (workspace#503, #504);
//! a target out of space grows like the engine, and added volumes join `_vinf`, the marker and the fsync pass (#508).
//! Source: decisions G1 workspace#481 (command surface, plan, marker; verify opt-in per ADR 0015), G4 workspace#485
//! (new log), G6 workspace#488 (LOB and Java assets), workspace#495 (verify), workspace#505 (this wiring).

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use fastmig_catalog::output_sql::UPDATE_STATISTICS_SQL;
use fastmig_copy::out_of_volume_asset::{self, AssetOutcome};
use fastmig_copy::volume_copy::IO_BUF_BYTES;
use fastmig_copy::volume_copy::preallocate_targets;
use fastmig_copy::{
    AsIs, CopyMethod, IncompleteMarker, VolumeJob, VolumeOutcome, copy_volumes, durable,
    lob_manifest, new_log, new_log_start, open_source, register_db, volinfo,
    volume_header_transform, volume_paths,
};
use fastmig_ondisk::Oid;
use fastmig_ondisk::boot_parm::BootDbParm;
use fastmig_ondisk::databases_txt::DatabaseEntry;
use fastmig_ondisk::file_alloc::SpaceManager;
use fastmig_ondisk::log_header::LogHeader;
use fastmig_ondisk::sector_reserve::{VolumeGrowth, sectors_of_bytes};
use fastmig_ondisk::source_version::HeaderLayout;
use fastmig_ondisk::volume_addition::VolumePlace;
use fastmig_ondisk::volume_set::{Access, Locate, VolumeSet};
use fastmig_plan::Plan;
use fastmig_plan::migration_plan::lob_base_dir;
use fastmig_plan::version_pair::line_name;
use fastmig_progress::{PhaseSpec, Progress, Unit, default_rates};
use fastmig_verify::Role;

use crate::args::{CliError, parse};
use crate::catalog_regeneration;
use crate::finalize::OUTSIDE_BASE;
use crate::limits::{self, Limits};
use crate::plan::{self as plan_cmd, OpenSource, PlanOptions, absolute, make_plan};
use crate::resource_estimate::{self, ResourceEstimate};
use crate::source_fingerprint::{self, SourceFingerprint};
use crate::target_space::{self, Reservation, TargetSpace};
use crate::verify::{DbArg, run_verifier};
use crate::{heap_counter, plan_report, progress_args, temp_disk_choice};
use fastmig_index::spill_arena::SpillArena;
use fastmig_index::temp_disk::TEMP_DISK;

const HELP: &str = "\
cubrid-fastmig migrate <db> --dest <dir> [options]
  --dest <dir>                   target directory for the volumes (must be empty or hold FASTMIG_INCOMPLETE;
                                 refused while another migrate holds its lock)
  --target-version 11.4|develop  target release line (default: develop; 11.4 is refused: no 11.4 layout
                                 B-tree builder)
  --log-dir <dir>                target log directory (default: --dest)
  --workers <n>                  worker limit: thread cap for the whole run (default: CPUs / 2)
  --target-name <name>           target DB name (default: <db>)
  --source-databases-txt <file>  default: $CUBRID_DATABASES/databases.txt
  --target-databases-txt <file>  default: $CUBRID_DATABASES/databases.txt
  --lob-dir <dir>                target lob-base-path (default: <dest>/lob)
  --mem <size>                   memory limit for the whole run, e.g. 16G (default: physical memory / 4)
  --spill-dir <dir>              where stages over the memory limit spill (default: <dest>/fastmig/spill)
  --temp-disk min|max|<size>     temp disk limit: spill bytes on disk at once, kept by building indexes in batches
                                 (default: asked on a terminal, else max when it fits the free space;
                                 docs/resources.md)
  --db-volume-size <size>        the target server's db_volume_size: size cap of a volume the run adds when the
                                 target runs out of space, e.g. 512M (default: 512M, the engine default)
  --verify                       run the verifier on target and source after the writes; register and remove
                                 FASTMIG_INCOMPLETE only with 0 migration defects (default: not run; finalize
                                 refuses a target without a passing verify)
  --no-clone                     byte-copy even where the filesystem could clone (measures the byte copy where clone would be chosen)
  --volumes-in-first-dir         find volumes 1.. next to volume 0 instead of at their recorded paths (a source
                                 copied to another directory without its headers rewritten, e.g. T1 pristine copies)";

const FLAGS: &[&str] = &[
    "--log-dir",
    "--workers",
    "--lob-dir",
    "--db-volume-size",
    "--temp-disk",
];
/// The run's phases, one per `Report::lap` in `migrate`, in order (GLOSSARY "phase").
const PHASES: &[PhaseSpec] = &[
    PhaseSpec::step("precheck"),
    PhaseSpec::step("open-sources"),
    PhaseSpec::work("volume-copy", Unit::Bytes, default_rates::COPY_BYTES),
    PhaseSpec::step("volume-paths"),
    PhaseSpec::work(
        "page-treatment",
        Unit::Count("pages"),
        default_rates::HEAP_PAGES,
    ),
    PhaseSpec::work(
        "index-build",
        Unit::Count("keys"),
        default_rates::INDEX_KEYS,
    ),
    PhaseSpec::step("catalog-regeneration"),
    PhaseSpec::step("out-of-volume-assets"),
    PhaseSpec::step("new-log"),
    PhaseSpec::work("durable-sync", Unit::Bytes, default_rates::FSYNC_BYTES),
    PhaseSpec::step("verifier"),
    PhaseSpec::step("register"),
];
const VOLUME_COPY: usize = 2;
/// Meter of the heap patch and re-encoding (workspace#503, #504): heap pages walked, total from the plan (#508).
const PAGE_TREATMENT: usize = 4;
/// Meter of the index build (workspace#503): keys done.
const INDEX_BUILD: usize = 5;
const DURABLE_SYNC: usize = 9;

const SWITCHES: &[&str] = &["--help", "--verify"];

struct Options {
    plan: PlanOptions,
    log_dir: PathBuf,
    worker_limit: usize,
    lob_dir: PathBuf,
    limits: Limits,
    /// `--verify`: run the verifier after the writes and register only a target with 0 migration defects (ADR 0015).
    verify: bool,
    /// The target server's `db_volume_size` in bytes: size cap of an added volume (ADR 0013).
    db_volume_size: u64,
}

pub fn run(argv: &[String]) -> Result<(), CliError> {
    let flags = [FLAGS, plan_cmd::FLAGS, limits::FLAGS, progress_args::FLAGS].concat();
    let p = parse(argv, &flags, &[SWITCHES, plan_cmd::SWITCHES].concat())?;
    if p.switch("--help") {
        println!("{HELP}\n{}", progress_args::HELP);
        return Ok(());
    }
    let plan = PlanOptions::from(&p, "migrate")?;
    let worker_limit = limits::worker_limit(&p)?;
    let temp_disk = limits::temp_disk_choice(&p)?;
    let o = Options {
        log_dir: p
            .value("--log-dir")
            .map(absolute)
            .transpose()?
            .unwrap_or_else(|| plan.target_dir.clone()),
        worker_limit,
        lob_dir: match p.value("--lob-dir") {
            Some(d) => absolute(d.strip_prefix("file:").unwrap_or(d))?,
            None => plan.target_dir.join("lob"),
        },
        limits: Limits::from(&p, &plan.target_dir)?,
        db_volume_size: match p.value("--db-volume-size") {
            Some(v) => limits::parse_size(v)
                .ok_or_else(|| CliError::Usage(format!("bad --db-volume-size {v}")))?,
            None => 512 << 20,
        },
        verify: p.switch("--verify"),
        plan,
    };
    // the copy engine streams every volume through one fixed buffer per worker
    if (o.worker_limit as u64).saturating_mul(IO_BUF_BYTES as u64) > o.limits.memory_limit_bytes {
        return Err(CliError::Usage(format!(
            "--workers {} needs {} KiB of copy buffers, over --mem",
            o.worker_limit,
            (o.worker_limit * IO_BUF_BYTES) >> 10
        )));
    }
    // The plan decides everything and refuses before anything is written, the temp disk limit included (asked on a
    // terminal before the progress display starts). The source log stays read-locked while `src` lives.
    let started = Instant::now();
    let src = OpenSource::open(&o.plan)?;
    let plan = make_plan(&o.plan, &src)?;
    if !plan.refusals.is_empty() {
        return Err(CliError::Failed(io::Error::other(format!(
            "the plan refuses the migration:\n  {}",
            plan.refusals.join("\n  ")
        ))));
    }
    let est = resource_estimate::estimate(
        &src.vols,
        plan.version_pair,
        plan.source_version,
        o.worker_limit,
        limits::physical_memory()?,
    )?;
    let mem = o.limits.memory_limit_bytes;
    let target = target_space::bound(
        &plan,
        &est,
        src.vols.io_pagesize() as u64,
        fs::metadata(&src.lgat_path)?.len(),
    );
    let temp_limit = temp_disk_choice::choose(
        temp_disk,
        &est,
        mem,
        o.verify,
        target.needed(),
        plan.available_bytes,
        &o.limits.spill_dir,
        &o.plan.target_dir,
    )?;
    let progress = Progress::new("migrate", PHASES, o.worker_limit);
    let monitor = progress_args::start(
        &p,
        progress.clone(),
        Some(o.limits.memory_limit_bytes),
        Some(temp_limit),
    )?;
    let report = migrate(
        &o,
        Begun {
            started,
            src,
            plan,
            est,
            temp_limit,
            target,
        },
        progress,
    )
    .map_err(CliError::Failed)?;
    monitor.finish();
    println!("{report}");
    Ok(())
}

/// Step timings for report.json.
struct Report {
    steps: Vec<(&'static str, f64)>,
    started: Instant,
    progress: Arc<Progress>,
}

impl Report {
    fn lap(&mut self, step: &'static str) {
        self.steps
            .push((step, self.started.elapsed().as_secs_f64()));
        self.started = Instant::now();
        self.progress.advance();
    }
}

/// Writes `text` to `path`, recorded in the marker first.
fn write_recorded(marker: &IncompleteMarker, path: &Path, text: &str) -> io::Result<()> {
    marker.record(path)?;
    fs::write(path, text)
}

/// What `run` decided before the progress display started: the open source (its log read-locked), the plan, the
/// resource estimate and the temp disk limit; `started` begins the precheck phase.
struct Begun {
    started: Instant,
    src: OpenSource,
    plan: Plan,
    est: ResourceEstimate,
    temp_limit: u64,
    target: TargetSpace,
}

/// Returns report.json.
fn migrate(o: &Options, b: Begun, progress: Arc<Progress>) -> io::Result<String> {
    let mut rep = Report {
        steps: Vec::new(),
        started: b.started,
        progress,
    };
    let po = &o.plan;
    let (src, plan) = (b.src, b.plan);
    let mem = o.limits.memory_limit_bytes;
    // one batch when the limit holds every index at once
    let mut limits = Limits {
        temp_disk: Some(b.temp_limit).filter(|&l| l < b.est.temp_max()),
        ..o.limits.clone()
    };
    let resources = b.est.json(mem);
    let temp_limit = b.temp_limit;
    let target = b.target;
    for w in &plan.warnings {
        rep.progress.warn(format!("warning: {w}"));
    }
    let pair = plan.version_pair;
    let src_lob_base = lob_base_dir(&src.entry).map_err(io::Error::other)?;
    let npages = BootDbParm::read(&src.vols)?.1.vacuum_log_block_npages;
    let first_pageid = new_log_start::first_log_pageid(src.log.append_lsa.pageid, npages as i64);
    rep.lap("precheck");

    let mut jobs = Vec::new();
    for v in &plan.volumes {
        let target = if v.volid == 0 {
            po.target_dir.join(&po.target_name)
        } else {
            po.target_dir
                .join(format!("{}_x{:03}", po.target_name, v.volid))
        };
        jobs.push(VolumeJob {
            volid: v.volid,
            source: open_source(&v.path)?,
            target,
            io_pagesize: src.vols.io_pagesize(),
            preallocated: v.copy_method == CopyMethod::Copy,
        });
    }
    let bytes = plan.volumes.iter().map(|v| v.bytes).sum();
    rep.progress.set_total(VOLUME_COPY, bytes);
    rep.progress.set_total(PAGE_TREATMENT, plan.treated_pages);
    // until the copy shows what was cloned, assume every byte is written and synced
    rep.progress.set_total(DURABLE_SYNC, bytes);
    let marker = IncompleteMarker::prepare(&po.target_dir)?;
    marker.create_dir_all(&o.log_dir)?;
    let fastmig_dir = po.target_dir.join("fastmig");
    marker.create_dir_all(&fastmig_dir)?;
    // every byte the run may write is taken from the filesystems now, before the first byte of the target
    // (workspace#526): byte-copied volumes in full, one reservation for the rest, the temp disk limit
    preallocate_targets(&jobs, &marker)?;
    let _reservation = Reservation::take(
        &marker,
        &fastmig_dir.join("reserve-target"),
        b.target.extra_bytes,
    )?;
    marker.create_dir_all(&limits.spill_dir)?;
    let arena = limits.spill_dir.join("arena");
    marker.record_scratch(&arena)?;
    limits.arena = Some(Arc::new(SpillArena::create(&arena, temp_limit).map_err(
        |e| {
            io::Error::other(format!(
                "temp disk: allocating {temp_limit} B at {}: {e}",
                arena.display()
            ))
        },
    )?));
    write_outputs(
        &marker,
        &fastmig_dir,
        &plan,
        &src,
        po,
        src_lob_base.as_deref(),
        &resources,
    )?;
    rep.lap("open_sources");

    let outcomes = copy_volumes(
        &jobs,
        o.worker_limit,
        &AsIs,
        po.allow_clone,
        &marker,
        rep.progress.meter(VOLUME_COPY),
    )?;
    let copied = outcomes
        .iter()
        .filter(|v| v.method == CopyMethod::Copy)
        .map(|v| v.bytes)
        .sum();
    rep.progress.set_total(DURABLE_SYNC, copied);
    rep.lap("volumes");
    let mut targets: Vec<PathBuf> = jobs.iter().map(|j| j.target.clone()).collect();
    if src.version.header_layout() == HeaderLayout::Before11_4 {
        // the 128 B volume header grows to 136 B before anything opens the target (workspace#529)
        let vol_creation = src.log.db_creation;
        volume_header_transform::transform(&targets, src.vols.io_pagesize(), vol_creation)?;
    }
    volume_paths::rewrite(&targets, src.vols.io_pagesize())?;
    rep.lap("volume_paths");

    // Heap patch and re-encoding (workspace#503, #504) fill this set with the classes whose heap header statistics
    // they measured (`HeapReport.measured`); verify compares those.
    let mut measured: BTreeSet<Oid> = BTreeSet::new();
    let vols = VolumeSet::open(
        &targets[0],
        Locate::Recorded,
        Access::ReadWrite,
        HeaderLayout::Since11_4,
    )?;
    let trk = BootDbParm::read(&vols)?.1.trk_vfid;
    // a target out of space grows like the engine would: volumes added next to volume 0 (ADR 0013, workspace#508)
    let growth = VolumeGrowth {
        nsect_vol_max: sectors_of_bytes(vols.io_pagesize(), o.db_volume_size),
        place: Some(VolumePlace {
            dir: po.target_dir.clone(),
            db_name: po.target_name.clone(),
            vinf: None,
        }),
    };
    let space = SpaceManager::new(&vols, pair.target, trk).with_growth(growth);
    marker.create_dir_all(&limits.spill_dir)?;
    // laps: page_treatment after prepare, index_build after the rebuild; the catalog writes and BT_STATS of finish
    // fall in catalog_regeneration, lapped after the closure
    let mut regenerate = || {
        if pair.regenerates_catalog() {
            let prepared = catalog_regeneration::prepare(
                &space,
                src.version,
                &src.log,
                src_lob_base.as_deref(),
                o.worker_limit,
                rep.progress.meter(PAGE_TREATMENT),
            )?;
            measured.extend(prepared.measured());
            rep.lap("page_treatment");
            let rebuilt = catalog_regeneration::build_indexes(
                &space,
                &prepared,
                &limits,
                o.worker_limit,
                rep.progress.meter(INDEX_BUILD),
            )?;
            rep.lap("index_build");
            catalog_regeneration::finish(&space, prepared, rebuilt)
        } else {
            // the same-version path: patch, deferred indexes out, every index rebuilt (workspace#501, #503, #504)
            let prepared = catalog_regeneration::same_version_prepare(
                &space,
                &src.log,
                rep.progress.meter(PAGE_TREATMENT),
            )?;
            measured.extend(prepared.measured());
            rep.lap("page_treatment");
            let rebuilt = catalog_regeneration::same_version_build_indexes(
                &space,
                &prepared,
                &limits,
                o.worker_limit,
                rep.progress.meter(INDEX_BUILD),
            )?;
            rep.lap("index_build");
            catalog_regeneration::same_version_finish(&space, prepared, rebuilt)
        }
    };
    let catalog = regenerate();
    // added volumes join the marker even when the phase failed, so a rerun removes them
    let added: Vec<PathBuf> = vols.volumes()[targets.len()..]
        .iter()
        .map(|v| v.path.clone())
        .collect();
    for p in &added {
        marker.record(p)?;
    }
    let catalog = catalog?;
    drop(space);
    drop(vols);
    let added_volumes = added.len();
    targets.extend(added);
    rep.lap("catalog_regeneration");

    let java = out_of_volume_asset::copy_dir_tree(
        &src.entry.vol_path.join("java"),
        &po.target_dir.join("java"),
        o.worker_limit,
        &marker,
    )?;
    let java_static = out_of_volume_asset::copy_dir_tree(
        &src.entry.vol_path.join("java_static"),
        &po.target_dir.join("java_static"),
        o.worker_limit,
        &marker,
    )?;
    let keys = out_of_volume_asset::copy_one(
        &src.entry.vol_path.join(format!("{}_keys", po.db)),
        &po.target_dir.join(format!("{}_keys", po.target_name)),
        &marker,
    )?;
    let lob = match &src_lob_base {
        Some(base) => out_of_volume_asset::copy_referenced_lob_files(
            base,
            &o.lob_dir,
            &plan.lob.referenced,
            &plan.lob.lob_dirs,
            o.worker_limit,
            &marker,
        )?,
        None => AssetOutcome::default(),
    };
    rep.lap("assets");

    // The new log comes last (workspace#482): a crash before it leaves no bootable target.
    let lgat = volinfo::log_dir_file(&o.log_dir, &po.target_name, "_lgat");
    marker.record(&lgat)?;
    new_log::write(
        &lgat,
        &new_log::new_header(&src.log, pair.target, first_pageid, &po.target_name),
    )?;
    marker.record(&volinfo::log_dir_file(&o.log_dir, &po.target_name, "_vinf"))?;
    volinfo::write(&o.log_dir, &po.target_name, &targets)?;
    rep.lap("new_log");
    durable::sync_created(&marker, o.worker_limit, rep.progress.meter(DURABLE_SYNC))?;
    rep.lap("fsync");

    // verify only on request (user decision 2026-10-30, ADR 0015): target rules on the target, source rules on the
    // source; the source log is not reopened (its lock)
    let measured_text: String = measured
        .iter()
        .map(|c| format!("{}|{}|{}\n", c.volid, c.pageid, c.slotid))
        .collect();
    write_recorded(
        &marker,
        &fastmig_dir.join("measured-classes"),
        &measured_text,
    )?;
    let verify_report = fastmig_dir.join("verify.jsonl");
    let (passed, summary) = if o.verify {
        marker.create_dir_all(&limits.spill_dir)?;
        marker.record(&verify_report)?;
        let (code, summary) = run_verifier(
            &DbArg {
                first_volume: targets[0].clone(),
                locate: Locate::Recorded,
                log: lgat.clone(),
                log_header: None,
                lob_base: Some(o.lob_dir.clone()),
            },
            Some(&DbArg {
                first_volume: src.entry.first_volume(),
                locate: po.locate,
                log: src.lgat_path.clone(),
                log_header: Some(src.log.clone()),
                lob_base: src_lob_base.clone(),
            }),
            Role::Target,
            &measured,
            o.worker_limit,
            &limits,
            &verify_report,
        )?;
        (code == 0, summary)
    } else {
        rep.progress
            .warn("verify: not run (pass --verify); finalize needs a passing verify".into());
        (true, "null".into())
    };
    rep.lap("verify");

    if passed {
        register_db::register(
            &po.target_dbtxt,
            &DatabaseEntry {
                name: po.target_name.clone(),
                vol_path: po.target_dir.clone(),
                hosts: src.entry.hosts.clone(),
                log_path: o.log_dir.clone(),
                lob_base_path: Some(format!("file:{}", o.lob_dir.display())),
            },
        )?;
    }
    rep.lap("register");

    let report = report_json(
        o,
        &rep,
        &plan,
        &src.log,
        npages,
        first_pageid,
        &outcomes,
        [&java, &java_static],
        keys.is_some(),
        &lob,
        &summary,
        &catalog.json(),
        added_volumes,
        &format!(
            "{{\"limit_bytes\": {}, \"batched\": {}, \"peak_bytes\": {}, \"target_copy_bytes\": {}, \"target_reserved_bytes\": {}}}",
            temp_limit,
            limits.temp_disk.is_some(),
            TEMP_DISK.now_and_peak().1,
            target.copy_bytes,
            target.extra_bytes
        ),
    );
    write_recorded(&marker, &fastmig_dir.join("report.json"), &report)?;
    if !passed {
        return Err(io::Error::other(format!(
            "verify found migration defects ({}); the target keeps FASTMIG_INCOMPLETE and is not registered\n{summary}",
            verify_report.display()
        )));
    }
    // the marker goes last: everything above is in place
    marker.finish()?;
    drop(src);
    Ok(report)
}

/// The plan's files in `<dest>/fastmig/`: what the user runs on the target, and what verify and finalize read.
fn write_outputs(
    marker: &IncompleteMarker,
    dir: &Path,
    plan: &Plan,
    src: &OpenSource,
    po: &PlanOptions,
    src_lob_base: Option<&Path>,
    resources: &str,
) -> io::Result<()> {
    write_recorded(
        marker,
        &dir.join("plan.json"),
        &plan_report::json(plan, resources),
    )?;
    write_recorded(
        marker,
        &dir.join("deferred-indexes.sql"),
        &plan.deferred_indexes_sql,
    )?;
    write_recorded(
        marker,
        &dir.join("recompile-plcsql.sql"),
        &plan.recompile_plcsql_sql,
    )?;
    write_recorded(
        marker,
        &dir.join("update-statistics.sql"),
        UPDATE_STATISTICS_SQL,
    )?;
    let manifest = dir.join("lob-manifest");
    marker.record(&manifest)?;
    lob_manifest::write(&manifest, &plan.lob.referenced)?;
    let outside: String = plan
        .lob
        .outside_base
        .iter()
        .map(|f| format!("{}\t{}\n", f.size, f.rel_path.display()))
        .collect();
    write_recorded(marker, &dir.join(OUTSIDE_BASE), &outside)?;
    let fp = SourceFingerprint::of(src, &po.source_dbtxt, src_lob_base)?;
    write_recorded(marker, &dir.join(source_fingerprint::FILE_NAME), &fp.text())
}

#[allow(clippy::too_many_arguments)]
fn report_json(
    o: &Options,
    rep: &Report,
    plan: &Plan,
    src_log: &LogHeader,
    npages: i32,
    first_pageid: i64,
    vols: &[VolumeOutcome],
    java: [&AssetOutcome; 2],
    keys: bool,
    lob: &AssetOutcome,
    verify_summary: &str,
    catalog: &str,
    added_volumes: usize,
    temp_disk: &str,
) -> String {
    let steps: Vec<String> = rep
        .steps
        .iter()
        .map(|(s, t)| format!("\"{s}\": {t:.3}"))
        .collect();
    let vols: Vec<String> = vols
        .iter()
        .map(|v| {
            format!(
                "{{\"volid\": {}, \"method\": \"{:?}\", \"bytes\": {}}}",
                v.volid, v.method, v.bytes
            )
        })
        .collect();
    let asset = |a: &AssetOutcome| {
        format!(
            "{{\"cloned\": {}, \"copied\": {}, \"bytes\": {}, \"missing\": {}}}",
            a.cloned,
            a.copied,
            a.bytes,
            a.missing.len()
        )
    };
    format!(
        "{{\n  \"version_pair\": {{\"source\": \"{}\", \"target\": \"{}\"}},\n  \"worker_limit\": {},\n  \"memory_limit_bytes\": {},\n  \"spill_dir\": {:?},\n  \"seconds\": {{{}}},\n  \"volumes\": [{}],\n  \"added_volumes\": {added_volumes},\n  \"java\": {},\n  \"java_static\": {},\n  \"keys_copied\": {keys},\n  \"lob\": {},\n  \"lob_orphans\": {{\"files\": {}, \"bytes\": {}}},\n  \"lob_absolute_under_base\": {},\n  \"source_append_lsa\": \"{}\",\n  \"vacuum_log_block_npages\": {npages},\n  \"new_log_first_pageid\": {first_pageid},\n  \"verify\": {verify_summary},\n  \"catalog\": {catalog},\n  \"heap_peak_bytes\": {},\n  \"temp_disk\": {temp_disk},\n  \"progress\": {}\n}}",
        plan.source_version.name(),
        line_name(plan.version_pair.target),
        o.worker_limit,
        o.limits.memory_limit_bytes,
        o.limits.spill_dir,
        steps.join(", "),
        vols.join(", "),
        asset(java[0]),
        asset(java[1]),
        asset(lob),
        plan.lob.orphans.len(),
        plan.lob.orphan_bytes(),
        plan.lob.absolute_under_base,
        src_log.append_lsa,
        heap_counter::now_and_peak().map_or("null".into(), |(_, peak)| peak.to_string()),
        fastmig_progress::report::json(&rep.progress),
    )
}
