//! `cubrid-fastmig migrate`: moves a cleanly shut down database to a new directory and registers it; with `--verify`
//! it verifies the target first.
//!
//! Input: the source DB name (found in the source `databases.txt`), the target directory and options.
//! Output: the target volumes, a new `_lgat`, `_vinf`, `_keys`, Java SP directories, referenced LOB files, and in
//! `<dest>/fastmig/`: `plan.json`, `report.json`, `verify.jsonl` (with `--verify`), the SQL files for the user
//! (`deferred-indexes.sql`, `recompile-plcsql.sql`, `post-migration-fixes.sql`, `update-statistics.sql`), `lob-manifest`, `lob-outside-base`,
//! `measured-classes` and `source-fingerprint` (read by finalize); then the target's `databases.txt` line. Progress
//! (the phases in `PHASES`) goes to stderr. The source is only read; its active log is read-locked for the whole run.
//! Order: the plan refuses before anything is written; the new log comes last of the writes; with `--verify` the
//! verifier runs on the durable target and only a target with 0 migration defects is registered and loses its
//! incomplete marker; without it every finished run is registered (ADR 0015) and `verify.jsonl` is not written.
//! Between the copy and the new log: page treatment, index rebuild and catalog regeneration (workspace#503, #504);
//! a target out of space grows like the engine, and added volumes join `_vinf`, the marker and the fsync pass (#508).
//! Source: decisions G1 workspace#481 (command surface, plan, marker; verify opt-in per ADR 0015), G4 workspace#485
//! (new log), G6 workspace#488 (LOB and Java assets), workspace#495 (verify), workspace#505 (this wiring).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use fastmig_catalog::output_sql::UPDATE_STATISTICS_SQL;
use fastmig_copy::byte_copy::{CopyRange, copy_range};
use fastmig_copy::out_of_volume_asset::{self, AssetOutcome};
use fastmig_copy::volume_copy::{CHUNK_BYTES, IO_BUF_BYTES, clone_targets, preallocate_targets};
use fastmig_copy::worker_pool;
use fastmig_copy::{
    AsIs, CopyMethod, IncompleteMarker, VolumeJob, VolumeOutcome, copy_volumes, durable,
    lob_manifest, new_log, new_log_start, open_source, register_db, volinfo,
    volume_header_transform, volume_paths,
};
use fastmig_ondisk::boot_parm::BootDbParm;
use fastmig_ondisk::databases_txt::DatabaseEntry;
use fastmig_ondisk::file_alloc::SpaceManager;
use fastmig_ondisk::log_header::LogHeader;
use fastmig_ondisk::preallocate;
use fastmig_ondisk::sector_reserve::{VolumeGrowth, sectors_of_bytes};
use fastmig_ondisk::sector_table::SECTOR_NPAGES;
use fastmig_ondisk::source_version::HeaderLayout;
use fastmig_ondisk::volume_addition::VolumePlace;
use fastmig_ondisk::volume_set::{Access, Locate, VolumeSet};
use fastmig_ondisk::{Oid, SourceVersion, Vsid};
use fastmig_plan::Plan;
use fastmig_plan::migration_plan::lob_base_dir;
use fastmig_plan::version_pair::line_name;
use fastmig_progress::{
    Bottleneck, DeviceThroughput, Meter, PhaseSpec, ProcSample, Progress, Unit, default_rates,
};
use fastmig_verify::Role;

use crate::args::{CliError, parse};
use crate::catalog_regeneration;
use crate::finalize::OUTSIDE_BASE;
use crate::limits::{self, Limits};
use crate::plan::{self as plan_cmd, OpenSource, PlanOptions, absolute, make_plan};
use crate::resource_estimate::{self, ResourceEstimate};
use crate::source_fingerprint::{self, SourceFingerprint};
use crate::target_space::{self, Reservation, TargetSpace};
use crate::verify::{self, DbArg, run_verifier};
use crate::{heap_counter, plan_report, progress_args, temp_disk_choice};
use fastmig_index::spill_arena::SpillArena;
use fastmig_index::temp_disk::TEMP_DISK;

const HELP: &str = "\
cubrid-fastmig migrate <db> --dest <dir> [options]
  --dest <dir>                   target directory for the volumes (must be empty or hold FASTMIG_INCOMPLETE;
                                 refused while another migrate holds its lock)
  --target-version 11.4|develop  target release line (default: develop; 11.4 takes a 10.2, 11.2, 11.3 or 11.4
                                 source)
  --log-dir <dir>                target log directory (default: --dest)
  --workers <n>                  worker limit: thread cap for the whole run (default: CPUs / 2)
  --target-name <name>           target DB name (default: <db>)
  --source-databases-txt <file>  default: $CUBRID_DATABASES/databases.txt
  --target-databases-txt <file>  default: $CUBRID_DATABASES/databases.txt
  --lob-dir <dir>                target lob-base-path (default: <dest>/lob)
  --mem <size>                   memory limit for the whole run, e.g. 16G (default: physical memory / 4, at least 256M)
  --spill-dir <dir>              where stages over the memory limit spill (default: <dest>/fastmig/spill)
  --temp-disk min|max|<size>     temp disk limit: spill bytes on disk at once, kept by building indexes in batches
                                 (default: asked on a terminal, else max when it fits the free space;
                                 docs/resources.md)
  --db-volume-size <size>        the target server's db_volume_size: size cap of a volume the run adds when the
                                 target runs out of space, e.g. 512M (default: 512M, the engine default)
  --verify                       run the verifier on target and source after the writes; register and remove
                                 FASTMIG_INCOMPLETE only with 0 migration defects (default: not run; finalize
                                 refuses a target without a passing verify)
  --source-conf <file>           the source server's cubrid.conf (see plan --help)
  --target-conf <file>           the target server's cubrid.conf (default: $CUBRID/conf/cubrid.conf)
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
    PhaseSpec::work(
        "open-sources",
        Unit::Bytes,
        default_rates::CLONED_RANGE_BYTES,
        Bottleneck::Write,
    ),
    PhaseSpec::work(
        "volume-copy",
        Unit::Bytes,
        default_rates::COPY_BYTES,
        Bottleneck::Write,
    ),
    PhaseSpec::step("volume-paths"),
    PhaseSpec::work(
        "page-treatment",
        Unit::Count("pages"),
        default_rates::HEAP_PAGES,
        Bottleneck::Cpu,
    ),
    PhaseSpec::parts(
        "index-build",
        Unit::Count("pages"),
        default_rates::INDEX_SCAN_PAGES,
        &[(Unit::Count("keys"), default_rates::TREE_BUILD_KEYS)],
        Bottleneck::Cpu,
    ),
    PhaseSpec::step("catalog-regeneration"),
    PhaseSpec::step("out-of-volume-assets"),
    PhaseSpec::step("new-log"),
    PhaseSpec::work(
        "durable-sync",
        Unit::Bytes,
        default_rates::FSYNC_BYTES,
        Bottleneck::Write,
    ),
    VERIFIER_SAME_LINE,
    PhaseSpec::step("register"),
];
/// The verifier's parts (workspace#560): the target's heap pages and pairs, then the source's, each side at its own
/// rate (a source of an older line reads and sorts slower: spilled runs, older records). `verify::run_verifier`
/// counts them in this order.
const VERIFIER_SAME_LINE: PhaseSpec = PhaseSpec::parts(
    "verifier",
    Unit::Count("pages"),
    default_rates::VERIFY_TARGET_HEAP_PAGES,
    &[
        (Unit::Count("pairs"), default_rates::VERIFY_TARGET_PAIRS),
        (
            Unit::Count("pages"),
            default_rates::VERIFY_SOURCE_HEAP_PAGES,
        ),
        (Unit::Count("pairs"), default_rates::VERIFY_SOURCE_PAIRS),
    ],
    Bottleneck::Cpu,
);
const VERIFIER_OLDER_LINE: PhaseSpec = PhaseSpec::parts(
    "verifier",
    Unit::Count("pages"),
    default_rates::VERIFY_TARGET_HEAP_PAGES,
    &[
        (Unit::Count("pairs"), default_rates::VERIFY_TARGET_PAIRS),
        (
            Unit::Count("pages"),
            default_rates::VERIFY_SOURCE_HEAP_PAGES_OLDER_LINE,
        ),
        (
            Unit::Count("pairs"),
            default_rates::VERIFY_SOURCE_PAIRS_OLDER_LINE,
        ),
    ],
    Bottleneck::Cpu,
);
/// Meter of taking the rewritten ranges of the clones (workspace#556, #560): bytes written, 0 on a byte copy.
const OPEN_SOURCES: usize = 1;
const VOLUME_COPY: usize = 2;
/// Meter of the heap patch and re-encoding (workspace#503, #504): heap pages walked, total from the plan (#508).
const PAGE_TREATMENT: usize = 4;
/// Meter of the index build (workspace#503): keys done.
const INDEX_BUILD: usize = 5;
const DURABLE_SYNC: usize = 9;
/// Meter of the verifier (workspace#560): index pairs merged, target and source.
const VERIFIER: usize = 10;

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
    // the copy engine streams every volume through one fixed buffer per worker; the index build merges and spills
    // through fixed buffers per worker too
    if limits::mem_min(o.worker_limit) > o.limits.memory_limit_bytes {
        return Err(CliError::Usage(format!(
            "--workers {} needs --mem {} KiB at least (copy buffers, index build merge and spill buffers)",
            o.worker_limit,
            limits::mem_min(o.worker_limit) >> 10
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
    // the reads inside precheck, timed on their own (workspace#526: what the space and temp figures cost)
    let mut pre: Vec<(&'static str, f64)> = Vec::new();
    let t = Instant::now();
    let before = ProcSample::now();
    let mut est = resource_estimate::estimate(
        &src.vols,
        plan.version_pair,
        plan.source_version,
        lob_base_dir(&src.entry)
            .map_err(io::Error::other)?
            .as_deref(),
        o.worker_limit,
        limits::physical_memory()?,
    )?;
    pre.push(("precheck.pair_count", t.elapsed().as_secs_f64()));
    let pair_count_used = ProcSample::now().zip(before).map(|(a, b)| a.since(b));
    pre.push(("precheck.appended_bound", est.appended_seconds));
    let t = Instant::now();
    est.source_groups = verify::source_key_groups(&src, o.plan.locate, o.worker_limit)?;
    pre.push(("precheck.source_key_groups", t.elapsed().as_secs_f64()));
    let mem = o.limits.memory_limit_bytes;
    if o.verify {
        let t = Instant::now();
        let (bytes, pages) = verify::source_pair_bytes(&src, o.plan.locate, o.worker_limit)?;
        pre.push(("precheck.verify_source_count", t.elapsed().as_secs_f64()));
        est.verify_source = Some(bytes);
        est.verify_heap_pages = Some(pages);
        // refused before anything is written: under it the verifier's scan workers get no share (workspace#526)
        let need = est.verify_mem_min().unwrap_or(0);
        if mem < need {
            return Err(CliError::Failed(io::Error::other(format!(
                "--verify with --workers {} needs --mem {} KiB at least (the verifier's live row map, merge floors and \
                 scan shares)",
                o.worker_limit,
                need >> 10
            ))));
        }
    }
    let target = target_space::bound(&plan, &est, &src.vols, new_log::file_bytes(&src.log))?;
    let asked = Instant::now();
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
    // the run's clock starts with the precheck, less the time a terminal question waited for its answer (workspace#560)
    let mut phases = PHASES.to_vec();
    if plan.version_pair.source != plan.version_pair.target {
        phases[VERIFIER] = VERIFIER_OLDER_LINE;
    }
    let progress = Progress::new(
        "migrate",
        &phases,
        &[],
        o.worker_limit,
        started + asked.elapsed(),
    );
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
            pre,
            pair_count_used,
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
    /// Timed parts of the precheck, reported before it.
    pre: Vec<(&'static str, f64)>,
    /// What the precheck's pair count used (CPU seconds, device bytes): the resource it was bound by.
    pair_count_used: Option<ProcSample>,
}

/// Returns report.json.
fn migrate(o: &Options, b: Begun, progress: Arc<Progress>) -> io::Result<String> {
    let mut rep = Report {
        steps: b.pre.clone(),
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
    // the verifier batches against its own max: one heap pass per database when the limit holds it (workspace#553)
    let verify_temp_disk =
        Some(b.temp_limit).filter(|&l| b.est.verify_max().is_some_and(|m| l < m));
    let resources = b.est.json(mem);
    let temp_limit = b.temp_limit;
    let target = b.target;
    let b_appended = b.est.appended_pages;
    let b_pairs: u64 = b.est.indexes.iter().map(|i| i.stats.pairs).sum();
    // the pair count reads the heaps and extracts the keys as the index build and the verifier will: the run's first
    // measure of this host's speed at that work (workspace#560). Chosen by the source line, as measured; shortcut: the
    // share of re-encoded classes is not weighed, until a DB mixing both is measured
    let count_rate = if plan.version_pair.source != plan.version_pair.target {
        default_rates::PAIR_COUNT_RE_ENCODED
    } else {
        default_rates::PAIR_COUNT
    }
    .with_workers(o.worker_limit);
    rep.progress.observe(
        b.est.scan_seconds - b.est.appended_seconds,
        b_pairs as f64 / count_rate,
        b.pair_count_used,
        DeviceThroughput::NVME,
    );
    for w in &plan.warnings {
        rep.progress.warn(format!("warning: {w}"));
    }
    let pair = plan.version_pair;
    let src_lob_base = lob_base_dir(&src.entry).map_err(io::Error::other)?;
    let npages = BootDbParm::read(&src.vols)?.1.vacuum_log_block_npages;
    let first_pageid = new_log_start::first_log_pageid(src.log.append_lsa.pageid, npages as i64);
    // every phase total before the precheck ends: the start estimate is made when the next phase begins
    let bytes: u64 = plan.volumes.iter().map(|v| v.bytes).sum();
    // a cloned volume costs its clone in open-sources, not a copy (workspace#560)
    let copy_bytes = (plan.volumes.iter())
        .filter(|v| v.copy_method == CopyMethod::Copy)
        .map(|v| v.bytes)
        .sum();
    rep.progress.set_total(VOLUME_COPY, copy_bytes);
    rep.progress.set_total(PAGE_TREATMENT, plan.treated_pages);
    // the sync goes over every file the run created, cloned volumes too (workspace#560)
    rep.progress
        .set_total(DURABLE_SYNC, bytes + new_log::file_bytes(&src.log));
    // the build scans the indexed heaps as the page treatment leaves them, then writes every pair to a leaf
    rep.progress
        .set_total(INDEX_BUILD, b.est.heap_pages() + b_appended);
    rep.progress.set_part_total(INDEX_BUILD, 1, b_pairs);
    if o.verify {
        // per side: every heap (the target's as many pages as the source's, grown by the page treatment), then every
        // pair once in the sort step (a run sorted, or a spilled run merged down) and once in the merge. shortcut: a
        // later index batch reads its heaps again, not counted, until a batched run is measured
        let source_pages = b.est.verify_heap_pages.unwrap_or(0) as u64;
        rep.progress.set_total(VERIFIER, source_pages + b_appended);
        rep.progress.set_part_total(VERIFIER, 1, 2 * b_pairs);
        rep.progress.set_part_total(VERIFIER, 2, source_pages);
        rep.progress.set_part_total(VERIFIER, 3, 2 * b_pairs);
    }
    if plan.unshare {
        let io = src.vols.io_pagesize() as u64;
        // the rewritten ranges: zeroing a range is one fallocate, not counted
        let pages: u64 = (target.unshare_ranges.iter()).map(|r| r.pages as u64).sum();
        rep.progress.set_total(OPEN_SOURCES, pages * io);
    }
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
            precloned: false,
        });
    }
    let marker = IncompleteMarker::prepare(&po.target_dir)?;
    marker.create_dir_all(&o.log_dir)?;
    let fastmig_dir = po.target_dir.join("fastmig");
    marker.create_dir_all(&fastmig_dir)?;
    // every byte the run may write is taken from the filesystems now, before the first byte of the target
    // (workspace#526): byte-copied volumes and the new log in full, one reservation for the rest, the temp disk limit
    preallocate_targets(&jobs, &marker)?;
    let lgat = volinfo::log_dir_file(&o.log_dir, &po.target_name, "_lgat");
    let log_header = new_log::new_header(&src.log, pair.target, first_pageid, &po.target_name);
    marker.record(&lgat)?;
    new_log::allocate(&lgat, &log_header)?;
    // cloned volumes now, and the pages the run rewrites in them unshared: their space is taken once, here
    let t = Instant::now();
    clone_targets(&mut jobs, &marker)?;
    if plan.unshare {
        take_cloned_ranges(
            &jobs,
            &target,
            src.vols.io_pagesize() as u64,
            o.worker_limit,
            rep.progress.meter(OPEN_SOURCES),
        )?;
    }
    rep.steps
        .push(("volumes.clone_unshare", t.elapsed().as_secs_f64()));
    let reservation = Arc::new(Reservation::take(
        &marker,
        &fastmig_dir.join("reserve-target"),
        target.extra_bytes + target.transfer_bytes,
    )?);
    marker.create_dir_all(&limits.spill_dir)?;
    let target_keys = po.target_dir.join(format!("{}_keys", po.target_name));
    if src.version == SourceVersion::V10_2 {
        // written by the catalog regeneration (TDE metadata, workspace#515)
        marker.record(&target_keys)?;
    }
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
        &plan_report::json(&plan, Ok((&target, resources.as_str()))),
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
    let mut space = SpaceManager::new(&vols, pair.target, trk).with_growth(growth);
    // a sector reserved off the list on a cloned volume, or a listed one of a file the run freed, takes its blocks when
    // reserved, the reservation's space handed over; failing that, the space change fails before a page of it is
    // written (workspace#526)
    let off_list = Arc::new(AtomicU64::new(0));
    if plan.unshare {
        let listed: HashSet<Vsid> = target.sectors.iter().copied().collect();
        let deferred: HashSet<Vsid> = target.deferred.iter().copied().collect();
        let res = reservation.clone();
        let mut cloned: HashMap<i16, (File, i32)> = HashMap::new();
        for (j, v) in jobs.iter().zip(&plan.volumes) {
            if v.copy_method == CopyMethod::Clone {
                let total = src.vols.volume(v.volid)?.header.nsect_total;
                cloned.insert(
                    v.volid,
                    (OpenOptions::new().write(true).open(&j.target)?, total),
                );
            }
        }
        let io_page = vols.io_pagesize() as u64;
        let sector = SECTOR_NPAGES as u64 * io_page;
        let count = off_list.clone();
        space = space.with_claim(Arc::new(move |s: Vsid| {
            let off_list = !listed.contains(&s);
            if !off_list && !deferred.contains(&s) {
                return Ok(());
            }
            if let Some((f, _)) = cloned.get(&s.volid).filter(|(_, total)| s.sectid < *total) {
                res.give(sector)?;
                preallocate::unallocated_pages(
                    f,
                    s.volid,
                    io_page,
                    s.sectid as u64 * sector,
                    sector,
                )?;
                if off_list {
                    count.fetch_add(1, Ordering::Relaxed);
                }
            }
            Ok(())
        }));
    }
    marker.create_dir_all(&limits.spill_dir)?;
    // laps: page_treatment after prepare, index_build after the rebuild; the catalog writes and BT_STATS of finish
    // fall in catalog_regeneration, lapped after the closure
    let mut regenerate = || {
        if pair.regenerates_catalog(src.version) {
            let prepared = catalog_regeneration::prepare(
                &space,
                &plan,
                &target_keys,
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
                pair.source,
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
    // new pages of every file (B-trees, catalog, appended heap pages): written outside the space taken at the start
    let allocated_pages = space.allocated_pages();
    drop(space);
    drop(vols);
    let added_volumes = added.len();
    let added_bytes: u64 = added
        .iter()
        .map(|p| fs::metadata(p).map_or(0, |m| m.len()))
        .sum();
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
        &target_keys,
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

    // The new log's header comes last (workspace#482): a crash before it leaves no bootable target.
    new_log::write(&lgat, &log_header)?;
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
            &Limits {
                temp_disk: verify_temp_disk,
                ..limits.clone()
            },
            &verify_report,
            Some(rep.progress.meter(VERIFIER)),
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
            "{{\"limit_bytes\": {}, \"batched\": {}, \"peak_bytes\": {}, \"target_copy_bytes\": {}, \"target_log_bytes\": {}, \"target_reserved_bytes\": {}, \"target_allocated_pages\": {allocated_pages}, \"target_btree_bound_pages\": {}, \"target_appended_bound_pages\": {}, \"target_added_volume_bytes\": {added_bytes}, \"target_listed_sectors\": {}, \"target_short_sectors\": {}, \"target_off_list_sectors\": {}}}",
            temp_limit,
            limits.temp_disk.is_some(),
            TEMP_DISK.now_and_peak().1,
            target.copy_bytes,
            target.log_bytes,
            target.extra_bytes,
            target.btree_pages,
            b_appended,
            target.sectors.len(),
            target.short_sectors,
            off_list.load(Ordering::Relaxed)
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
    plan_json: &str,
) -> io::Result<()> {
    write_recorded(marker, &dir.join("plan.json"), plan_json)?;
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
        &dir.join("post-migration-fixes.sql"),
        &plan.post_migration_fixes_sql,
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

/// Gives the ranges of the cloned volumes the run writes blocks of their own, before anything else is written: a
/// rewritten range gets the source's bytes written over it, a range whose content the run does not need is zeroed
/// and formatted as unallocated pages (`preallocate::unallocated_pages`, workspace#591). A whole-block write into a
/// clone takes new blocks without reading the shared ones, from the source's page cache and on every worker; `FALLOC_FL_UNSHARE_RANGE` read them back from disk a page at a time,
/// one worker per volume, about 10 times slower (workspace#556). XFS reserves the blocks of a buffered write into a
/// clone before the write returns, and the step ends with an fdatasync of every cloned volume, so a full disk fails
/// here, before the first rewrite (workspace#526).
fn take_cloned_ranges(
    jobs: &[VolumeJob],
    target: &TargetSpace,
    io: u64,
    worker_limit: usize,
    meter: Meter,
) -> io::Result<()> {
    let files = jobs
        .iter()
        .map(|j| OpenOptions::new().write(true).open(&j.target))
        .collect::<io::Result<Vec<File>>>()?;
    // (job index, range, zero), every range cut into copy-engine chunks so all workers share the big ones (a zero
    // range too: ZERO_RANGE and two header writes per page, measured as fast as one task per range, workspace#593)
    let mut tasks: Vec<(usize, u64, u64, bool)> = Vec::new();
    let ranges = (target.unshare_ranges.iter().map(|r| (r, false)))
        .chain(target.zero_ranges.iter().map(|r| (r, true)));
    for (r, zero) in ranges {
        let j = jobs
            .iter()
            .position(|j| j.volid == r.volid)
            .ok_or_else(|| io::Error::other(format!("no target for volume {}", r.volid)))?;
        let (start, end) = (r.first as u64 * io, (r.first as u64 + r.pages as u64) * io);
        tasks.extend(
            (start..end)
                .step_by(CHUNK_BYTES as usize)
                .map(|s| (j, s, end.min(s + CHUNK_BYTES), zero)),
        );
    }
    worker_pool::run(tasks.len(), worker_limit, |i| {
        let (j, start, end, zero) = tasks[i];
        let done = if zero {
            preallocate::unallocated_pages(&files[j], jobs[j].volid, io, start, end - start)
        } else {
            let mut buf = vec![0u8; IO_BUF_BYTES];
            let range = CopyRange {
                src: &jobs[j].source,
                dst: &files[j],
                volid: jobs[j].volid,
                io_pagesize: io as usize,
                start,
                end,
            };
            let done = copy_range(&range, &mut buf, &AsIs);
            meter.add(end - start);
            done
        };
        done.map_err(|e| {
            io::Error::new(
                e.kind(),
                format!(
                    "taking bytes {start}..{end} of {}: {e}",
                    jobs[j].target.display()
                ),
            )
        })
    })?;
    // a delayed write error (ENOSPC included) shows here, before the first rewrite of these ranges
    worker_pool::run(files.len(), worker_limit, |i| {
        files[i].sync_data().map_err(|e| {
            io::Error::new(
                e.kind(),
                format!("syncing {}: {e}", jobs[i].target.display()),
            )
        })
    })
}
