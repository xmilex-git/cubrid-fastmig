//! `cubrid-fastmig migrate`: moves a cleanly shut down database to a new directory and registers it.
//!
//! Input: the source DB name (found in the source `databases.txt`), the target directory and options.
//! Output: the target volumes, a new `_lgat`, `_vinf`, `_keys`, Java SP directories, referenced LOB files,
//! `<dest>/fastmig/report.json`, and the target's `databases.txt` line. The source is only read; its active log is read-locked for the whole run.
//! Scope today: the same-version path only (source line = target line, pages as is). Page treatment, catalog
//! regeneration, index rebuild and verify are added by the P1 integration (workspace#484) on this skeleton; another
//! version pair is refused.
//! Source: decisions G1 workspace#481 (command surface, registration, marker), G4 workspace#485 (new log),
//! G6 workspace#488 (LOB and Java assets).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use fastmig_copy::asset_copy::{self, AssetOutcome};
use fastmig_copy::{
    IncompleteMarker, NoPatch, VolumeJob, VolumeOutcome, copy_volumes, lob_manifest, new_log,
    new_log_start, open_locked_log, open_source, register_db, volinfo, volume_paths,
};
use fastmig_ondisk::VersionLine;
use fastmig_ondisk::boot_parm::BootDbParm;
use fastmig_ondisk::databases_txt::{self, DatabaseEntry};
use fastmig_ondisk::log_header::{self, LogHeader};
use fastmig_ondisk::volume_set::{Access, Locate, VolumeSet};

use crate::args::{CliError, parse};

const HELP: &str = "\
cubrid-fastmig migrate <db> --dest <dir> [options]
  --dest <dir>                   target directory for the volumes (must be empty or hold FASTMIG_INCOMPLETE)
  --log-dir <dir>                target log directory (default: --dest)
  --workers <n>                  thread cap for the whole run (default: CPUs / 2)
  --target-name <name>           target DB name (default: <db>)
  --source-databases-txt <file>  default: $CUBRID_DATABASES/databases.txt
  --target-databases-txt <file>  default: $CUBRID_DATABASES/databases.txt
  --lob-dir <dir>                target lob-base-path (default: <dest>/lob)
  --lob-manifest <file>          referenced LOB files (<size>\\t<relative path> per line); without it no LOB
                                 file is copied (the locator scan that builds it comes with workspace#484)
  --no-clone                     byte-copy even where the filesystem could clone (copy-mode measurement)
  --volumes-in-first-dir         find volumes 1.. next to volume 0 instead of at their recorded paths (a source
                                 copied to another directory without its headers rewritten, e.g. T1 pristine copies)";

const FLAGS: &[&str] = &[
    "--dest",
    "--log-dir",
    "--workers",
    "--target-name",
    "--source-databases-txt",
    "--target-databases-txt",
    "--lob-dir",
    "--lob-manifest",
];
const SWITCHES: &[&str] = &["--no-clone", "--volumes-in-first-dir", "--help"];

struct Options {
    db: String,
    dest: PathBuf,
    log_dir: PathBuf,
    workers: usize,
    target_name: String,
    source_dbtxt: PathBuf,
    target_dbtxt: PathBuf,
    lob_dir: PathBuf,
    lob_manifest: Option<PathBuf>,
    allow_clone: bool,
    locate: Locate,
}

pub fn run(argv: &[String]) -> Result<(), CliError> {
    let p = parse(argv, FLAGS, SWITCHES)?;
    if p.switch("--help") {
        println!("{HELP}");
        return Ok(());
    }
    let [db] = p.positional.as_slice() else {
        return Err(CliError::Usage(
            "migrate takes exactly one database name".into(),
        ));
    };
    let dest = absolute(
        p.value("--dest")
            .ok_or_else(|| CliError::Usage("--dest is required".into()))?,
    )?;
    let default_dbtxt = || -> Result<PathBuf, CliError> {
        let dir = std::env::var_os("CUBRID_DATABASES").ok_or_else(|| {
            CliError::Usage("CUBRID_DATABASES is not set; pass the databases.txt path".into())
        })?;
        Ok(PathBuf::from(dir).join("databases.txt"))
    };
    let workers = match p.value("--workers") {
        Some(w) => w
            .parse()
            .ok()
            .filter(|&n| n > 0)
            .ok_or_else(|| CliError::Usage(format!("bad --workers {w}")))?,
        None => std::thread::available_parallelism().map_or(1, |n| (n.get() / 2).max(1)),
    };
    let o = Options {
        db: db.clone(),
        log_dir: p
            .value("--log-dir")
            .map(absolute)
            .transpose()?
            .unwrap_or_else(|| dest.clone()),
        workers,
        target_name: p.value("--target-name").unwrap_or(db).to_string(),
        source_dbtxt: p
            .value("--source-databases-txt")
            .map(PathBuf::from)
            .map_or_else(default_dbtxt, Ok)?,
        target_dbtxt: p
            .value("--target-databases-txt")
            .map(PathBuf::from)
            .map_or_else(default_dbtxt, Ok)?,
        lob_dir: match p.value("--lob-dir") {
            Some(d) => absolute(d.strip_prefix("file:").unwrap_or(d))?,
            None => dest.join("lob"),
        },
        lob_manifest: p.value("--lob-manifest").map(PathBuf::from),
        allow_clone: !p.switch("--no-clone"),
        locate: if p.switch("--volumes-in-first-dir") {
            Locate::FirstVolumeDir
        } else {
            Locate::Recorded
        },
        dest,
    };
    migrate(&o).map_err(CliError::Failed)
}

fn absolute(p: &str) -> Result<PathBuf, CliError> {
    std::path::absolute(p).map_err(CliError::Failed)
}

/// Step timings and outcomes for report.json.
struct Report {
    steps: Vec<(&'static str, f64)>,
    started: Instant,
}

impl Report {
    fn lap(&mut self, step: &'static str) {
        self.steps
            .push((step, self.started.elapsed().as_secs_f64()));
        self.started = Instant::now();
    }
}

fn migrate(o: &Options) -> io::Result<()> {
    let mut rep = Report {
        steps: Vec::new(),
        started: Instant::now(),
    };

    // Pre-check: source registered, cleanly shut down, one consistent volume set, a supported pair.
    let src = databases_txt::lookup(&o.source_dbtxt, &o.db)?;
    let src_lob_base = match &src.lob_base_path {
        Some(l) => Some(PathBuf::from(l.strip_prefix("file:").ok_or_else(|| {
            io::Error::other(format!("lob-base-path {l} is not file: (workspace#488)"))
        })?)),
        None => None,
    };
    // The source log stays open and read-locked until this function returns (dropping `src_lgat` releases it);
    // a source whose server is up is refused here, before anything is written.
    let src_lgat_path = src.log_path.join(format!("{}_lgat", o.db));
    let src_lgat = open_locked_log(&src_lgat_path)?;
    let src_log = read_log_header(&src_lgat, &src_lgat_path)?;
    let vols = VolumeSet::open_entry(&src, o.locate, Access::ReadOnly)?;
    let line = precheck(&src_log, &vols)?;
    let npages = BootDbParm::read(&vols)?.1.vacuum_log_block_npages;
    let first_pageid = new_log_start::first_log_pageid(src_log.append_lsa.pageid, npages as i64);
    register_db::check_name_free(&o.target_dbtxt, &o.target_name)?;
    rep.lap("precheck");

    let mut jobs = Vec::new();
    for v in vols.volumes() {
        let target = if v.volid == 0 {
            o.dest.join(&o.target_name)
        } else {
            o.dest.join(format!("{}_x{:03}", o.target_name, v.volid))
        };
        jobs.push(VolumeJob {
            volid: v.volid,
            source: open_source(&v.path, true)?,
            target,
            io_pagesize: vols.io_pagesize(),
        });
    }
    let marker = IncompleteMarker::prepare(&o.dest)?;
    marker.create_dir_all(&o.log_dir)?;
    rep.lap("open_sources");

    let outcomes = copy_volumes(&jobs, o.workers, &NoPatch, o.allow_clone, &marker)?;
    rep.lap("volumes");
    let targets: Vec<PathBuf> = jobs.iter().map(|j| j.target.clone()).collect();
    volume_paths::rewrite(&targets, vols.io_pagesize())?;
    rep.lap("volume_paths");

    let java = asset_copy::copy_dir_tree(
        &src.vol_path.join("java"),
        &o.dest.join("java"),
        o.workers,
        &marker,
    )?;
    let java_static = asset_copy::copy_dir_tree(
        &src.vol_path.join("java_static"),
        &o.dest.join("java_static"),
        o.workers,
        &marker,
    )?;
    let keys = asset_copy::copy_one(
        &src.vol_path.join(format!("{}_keys", o.db)),
        &o.dest.join(format!("{}_keys", o.target_name)),
        &marker,
    )?;
    let fastmig_dir = o.dest.join("fastmig");
    marker.create_dir_all(&fastmig_dir)?;
    let lob = copy_lobs(o, src_lob_base.as_deref(), &fastmig_dir, &marker)?;
    rep.lap("assets");

    // The new log comes last (workspace#482): a crash before it leaves no bootable target.
    let lgat = volinfo::log_dir_file(&o.log_dir, &o.target_name, "_lgat");
    marker.record(&lgat)?;
    new_log::write(
        &lgat,
        &new_log::new_header(&src_log, line, first_pageid, &o.target_name),
    )?;
    marker.record(&volinfo::log_dir_file(&o.log_dir, &o.target_name, "_vinf"))?;
    volinfo::write(&o.log_dir, &o.target_name, &targets)?;
    rep.lap("new_log");

    register_db::register(
        &o.target_dbtxt,
        &DatabaseEntry {
            name: o.target_name.clone(),
            vol_path: o.dest.clone(),
            hosts: src.hosts.clone(),
            log_path: o.log_dir.clone(),
            lob_base_path: Some(format!("file:{}", o.lob_dir.display())),
        },
    )?;
    rep.lap("register");

    let report = report_json(
        o,
        &rep,
        &src_log,
        npages,
        first_pageid,
        &outcomes,
        [&java, &java_static],
        keys.is_some(),
        &lob,
    );
    let report_path = fastmig_dir.join("report.json");
    marker.record(&report_path)?;
    fs::write(&report_path, &report)?;
    println!("{report}");
    // FASTMIG_INCOMPLETE stays: verify (workspace#495) removes it once the target passes.
    drop(src_lgat);
    Ok(())
}

/// Reads the header through the locked handle `f`: opening and closing another descriptor would drop the lock.
fn read_log_header(f: &fs::File, path: &Path) -> io::Result<LogHeader> {
    let mut page = vec![0u8; 512];
    std::os::unix::fs::FileExt::read_exact_at(f, &mut page, 0)?;
    let h = LogHeader::read(&page);
    if h.magic != log_header::MAGIC {
        return Err(io::Error::other(format!(
            "{}: not an active log",
            path.display()
        )));
    }
    Ok(h)
}

/// The refusals this path can check (G1 pre-check); returns the release line of source and target.
fn precheck(log: &LogHeader, vols: &VolumeSet) -> io::Result<VersionLine> {
    if !log.is_shutdown {
        return Err(io::Error::other(
            "the source was not shut down cleanly (log header is_shutdown = 0)",
        ));
    }
    let line = VersionLine::from_db_compatibility(log.db_compatibility).ok_or_else(|| {
        io::Error::other(format!(
            "unsupported source db_compatibility {}",
            log.db_compatibility
        ))
    })?;
    // shortcut: the same-version path only; other pairs need the treatments workspace#484 adds
    if log.db_iopagesize as usize != vols.io_pagesize() {
        return Err(io::Error::other("log header and volume iopagesize differ"));
    }
    for v in vols.volumes() {
        if v.header.db_creation != log.db_creation {
            return Err(io::Error::other(format!(
                "{}: db_creation {} differs from the log's {}; not one database",
                v.path.display(),
                v.header.db_creation,
                log.db_creation
            )));
        }
    }
    Ok(line)
}

fn copy_lobs(
    o: &Options,
    src_base: Option<&Path>,
    fastmig_dir: &Path,
    marker: &IncompleteMarker,
) -> io::Result<AssetOutcome> {
    let Some(manifest) = &o.lob_manifest else {
        if src_base.is_some_and(Path::exists) {
            eprintln!(
                "warning: no --lob-manifest: LOB files are not copied (locator scan lands with workspace#484)"
            );
        }
        return Ok(AssetOutcome::default());
    };
    let src_base = src_base.ok_or_else(|| {
        io::Error::other("--lob-manifest given but the source has no lob-base-path")
    })?;
    let entries = lob_manifest::read(manifest)?;
    let out = asset_copy::copy_lob_files(src_base, &o.lob_dir, &entries, o.workers, marker)?;
    let target_manifest = fastmig_dir.join("lob-manifest");
    marker.record(&target_manifest)?;
    lob_manifest::write(&target_manifest, &entries)?;
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn report_json(
    o: &Options,
    rep: &Report,
    src_log: &LogHeader,
    npages: i32,
    first_pageid: i64,
    vols: &[VolumeOutcome],
    java: [&AssetOutcome; 2],
    keys: bool,
    lob: &AssetOutcome,
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
        "{{\n  \"workers\": {},\n  \"seconds\": {{{}}},\n  \"volumes\": [{}],\n  \"java\": {},\n  \"java_static\": {},\n  \"keys_copied\": {keys},\n  \"lob\": {},\n  \"source_append_lsa\": \"{}\",\n  \"vacuum_log_block_npages\": {npages},\n  \"new_log_first_pageid\": {first_pageid}\n}}",
        o.workers,
        steps.join(", "),
        vols.join(", "),
        asset(java[0]),
        asset(java[1]),
        asset(lob),
        src_log.append_lsa,
    )
}
