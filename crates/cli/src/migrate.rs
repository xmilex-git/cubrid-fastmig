//! `cubrid-fastmig migrate`: moves a cleanly shut down database to a new directory, verifies it, and registers it.
//!
//! Input: the source DB name (found in the source `databases.txt`), the target directory and options.
//! Output: the target volumes, a new `_lgat`, `_vinf`, `_keys`, Java SP directories, referenced LOB files, and in
//! `<dest>/fastmig/`: `plan.json`, `report.json`, `verify.jsonl`, the SQL files for the user
//! (`deferred-indexes.sql`, `recompile-plcsql.sql`, `update-statistics.sql`), `lob-manifest`, `lob-outside-base`,
//! `measured-classes` and `source-fingerprint` (read by finalize); then the target's `databases.txt` line. Progress
//! (the phases in `PHASES`) goes to stderr. The source is only read; its active log is read-locked for the whole run.
//! Order: the plan refuses before anything is written; the new log comes last of the writes; verify runs on the
//! durable target; only a target with 0 migration defects is registered and loses its incomplete marker.
//! Scope today: the plan's version pair is a same-version pair (pages copied as is). Page treatment, index rebuild
//! and catalog regeneration are wired by workspace#503 and #504 into their phases here.
//! Source: decisions G1 workspace#481 (command surface, plan, registration after verify, marker), G4 workspace#485
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
use fastmig_copy::{
    AsIs, CopyMethod, IncompleteMarker, VolumeJob, VolumeOutcome, copy_volumes, durable,
    lob_manifest, new_log, new_log_start, open_source, register_db, volinfo, volume_paths,
};
use fastmig_ondisk::Oid;
use fastmig_ondisk::boot_parm::BootDbParm;
use fastmig_ondisk::databases_txt::DatabaseEntry;
use fastmig_ondisk::log_header::LogHeader;
use fastmig_ondisk::volume_set::Locate;
use fastmig_plan::Plan;
use fastmig_plan::migration_plan::lob_base_dir;
use fastmig_plan::version_pair::line_name;
use fastmig_progress::{PhaseSpec, Progress, Unit, default_rates};
use fastmig_verify::Role;

use crate::args::{CliError, parse};
use crate::finalize::OUTSIDE_BASE;
use crate::limits::{self, Limits};
use crate::plan::{self as plan_cmd, OpenSource, PlanOptions, absolute, make_plan};
use crate::source_fingerprint::{self, SourceFingerprint};
use crate::verify::{DbArg, run_verifier};
use crate::{heap_counter, plan_report, progress_args};

const HELP: &str = "\
cubrid-fastmig migrate <db> --dest <dir> [options]
  --dest <dir>                   target directory for the volumes (must be empty or hold FASTMIG_INCOMPLETE)
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
  --no-clone                     byte-copy even where the filesystem could clone (measures the byte copy where clone would be chosen)
  --volumes-in-first-dir         find volumes 1.. next to volume 0 instead of at their recorded paths (a source
                                 copied to another directory without its headers rewritten, e.g. T1 pristine copies)";

const FLAGS: &[&str] = &["--log-dir", "--workers", "--lob-dir"];
/// The run's phases, one per `Report::lap` in `migrate`, in order (GLOSSARY "phase").
const PHASES: &[PhaseSpec] = &[
    PhaseSpec::step("precheck"),
    PhaseSpec::step("open-sources"),
    PhaseSpec::work("volume-copy", Unit::Bytes, default_rates::COPY_BYTES),
    PhaseSpec::step("volume-paths"),
    PhaseSpec::work(
        "page-treatment",
        Unit::Count("records"),
        default_rates::REENCODE_RECORDS,
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
/// Meter of the heap patch and re-encoding (workspace#503, #504): records done.
#[allow(dead_code)]
const PAGE_TREATMENT: usize = 4;
/// Meter of the index build (workspace#503): keys done.
#[allow(dead_code)]
const INDEX_BUILD: usize = 5;
const DURABLE_SYNC: usize = 9;

const SWITCHES: &[&str] = &["--help"];

struct Options {
    plan: PlanOptions,
    log_dir: PathBuf,
    worker_limit: usize,
    lob_dir: PathBuf,
    limits: Limits,
}

pub fn run(argv: &[String]) -> Result<(), CliError> {
    let flags = [FLAGS, plan_cmd::FLAGS, limits::FLAGS, progress_args::FLAGS].concat();
    let p = parse(argv, &flags, &[SWITCHES, plan_cmd::SWITCHES].concat())?;
    if p.switch("--help") {
        println!("{HELP}\n{}", progress_args::HELP);
        return Ok(());
    }
    let plan = PlanOptions::from(&p, "migrate")?;
    let worker_limit = match p.value("--workers") {
        Some(w) => w
            .parse()
            .ok()
            .filter(|&n| n > 0)
            .ok_or_else(|| CliError::Usage(format!("bad --workers {w}")))?,
        None => std::thread::available_parallelism().map_or(1, |n| (n.get() / 2).max(1)),
    };
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
    let progress = Progress::new("migrate", PHASES, o.worker_limit);
    let monitor = progress_args::start(&p, progress.clone(), Some(o.limits.memory_limit_bytes))?;
    let report = migrate(&o, progress).map_err(CliError::Failed)?;
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

/// Returns report.json.
fn migrate(o: &Options, progress: Arc<Progress>) -> io::Result<String> {
    let mut rep = Report {
        steps: Vec::new(),
        started: Instant::now(),
        progress,
    };
    let po = &o.plan;

    // The plan decides everything and refuses before anything is written. The source log stays read-locked until
    // this function returns (`src` owns the handle).
    let src = OpenSource::open(po)?;
    let plan = make_plan(po, &src)?;
    if !plan.refusals.is_empty() {
        return Err(io::Error::other(format!(
            "the plan refuses the migration:\n  {}",
            plan.refusals.join("\n  ")
        )));
    }
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
        });
    }
    let bytes = plan.volumes.iter().map(|v| v.bytes).sum();
    rep.progress.set_total(VOLUME_COPY, bytes);
    // until the copy shows what was cloned, assume every byte is written and synced
    rep.progress.set_total(DURABLE_SYNC, bytes);
    let marker = IncompleteMarker::prepare(&po.target_dir)?;
    marker.create_dir_all(&o.log_dir)?;
    let fastmig_dir = po.target_dir.join("fastmig");
    marker.create_dir_all(&fastmig_dir)?;
    write_outputs(
        &marker,
        &fastmig_dir,
        &plan,
        &src,
        po,
        src_lob_base.as_deref(),
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
    let targets: Vec<PathBuf> = jobs.iter().map(|j| j.target.clone()).collect();
    volume_paths::rewrite(&targets, src.vols.io_pagesize())?;
    rep.lap("volume_paths");

    // Heap patch and re-encoding (workspace#503, #504) fill this set with the classes whose heap header statistics
    // they measured (`HeapReport.measured`); verify compares those.
    let measured: BTreeSet<Oid> = BTreeSet::new();
    rep.lap("page_treatment");
    rep.lap("index_build");
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

    // verify: target rules on the target, source rules on the source; the source log is not reopened (its lock)
    let measured_text: String = measured
        .iter()
        .map(|c| format!("{}|{}|{}\n", c.volid, c.pageid, c.slotid))
        .collect();
    write_recorded(
        &marker,
        &fastmig_dir.join("measured-classes"),
        &measured_text,
    )?;
    marker.create_dir_all(&o.limits.spill_dir)?;
    let verify_report = fastmig_dir.join("verify.jsonl");
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
        &o.limits,
        &verify_report,
    )?;
    rep.lap("verify");

    let passed = code == 0;
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
) -> io::Result<()> {
    write_recorded(marker, &dir.join("plan.json"), &plan_report::json(plan))?;
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
        "{{\n  \"version_pair\": {{\"source\": \"{}\", \"target\": \"{}\"}},\n  \"worker_limit\": {},\n  \"memory_limit_bytes\": {},\n  \"spill_dir\": {:?},\n  \"seconds\": {{{}}},\n  \"volumes\": [{}],\n  \"java\": {},\n  \"java_static\": {},\n  \"keys_copied\": {keys},\n  \"lob\": {},\n  \"lob_orphans\": {{\"files\": {}, \"bytes\": {}}},\n  \"lob_absolute_under_base\": {},\n  \"source_append_lsa\": \"{}\",\n  \"vacuum_log_block_npages\": {npages},\n  \"new_log_first_pageid\": {first_pageid},\n  \"verify\": {verify_summary},\n  \"heap_peak_bytes\": {},\n  \"progress\": {}\n}}",
        line_name(plan.version_pair.source),
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
