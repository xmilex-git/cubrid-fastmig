//! `cubrid-fastmig plan`: reads the source only and prints what `migrate` would do; also the source opening and the
//! plan building `migrate` shares, so both decide the same way.
//!
//! Input: the source DB name (found in the source `databases.txt`), the optional new DB name, the target directory
//! and the choices (`--target-version`, `--no-clone`, `-r`, `-E`/`-i`, the target conf defaults). Output: the plan
//! as a table on stdout, or JSON with `--json`; exit code 1 when the plan has refusals.
//! Names and defaults follow copydb/createdb and the target `cubrid.conf` (G13 workspace#604): `<db> [<new-db>]`,
//! `-F`, the target line from the target install, `--db-volume-size`/`--temp-mem-size`/`--temp-disk-size` from the
//! conf; the command line always wins. The chosen values are the `settings` object of plan.json.
//! Source: decision G1 workspace#481 (plan writes nothing; refusals and warnings), workspace#505 (`--target-version`
//! decided once by the plan), G13 workspace#604.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use fastmig_copy::{incomplete_marker, new_log, open_locked_log, register_db};
use fastmig_ondisk::databases_txt::{self, DatabaseEntry};
use fastmig_ondisk::log_header::{self, LogHeader};
use fastmig_ondisk::volume_set::{Access, Locate, VolumeSet};
use fastmig_ondisk::{SourceVersion, VersionLine};
use fastmig_plan::cubrid_conf::ignore_trailing_space;
use fastmig_plan::migration_plan::lob_base_dir;
use fastmig_plan::version_pair::{line_name, parse_line};
use fastmig_plan::{Plan, PlanInputs};

use crate::args::{CliError, Parsed, parse};
use crate::limits::{self, Limits, TempDiskChoice};
use crate::replace_target::{self, ReplacedDatabase};
use crate::storage_kind_detect::RunStorage;
use crate::target_conf_defaults::TargetConfDefaults;
use crate::volume_targets::{self, VolumeLayout};
use crate::{
    check, plan_report, resource_estimate, target_install_version, target_space, volume_dirs,
};

pub const HELP: &str = "\
cubrid-fastmig plan <db> [<new-db>] -F <dir> [options]
  Reads the source only and prints what migrate would do (exit 1: the plan refuses).
  <new-db>                       target DB name (default: <db>)
  -F, --file-path <dir>          target directory migrate would write (volume 0, fastmig/)
  --target-version 11.4|develop  target release line (default: the line of the install $CUBRID; 11.4 takes a 10.2,
                                 11.2, 11.3 or 11.4 source)
  -E, --extended-volume-path <dir>  target directory of volumes 1.. (default: -F); not with -i
  -i, --control-file <file>      target path per volume, copydb's format: `volid source_fullvolname dest_fullvolname`
                                 lines (volumes it does not list go to -F); not with -E
  -r, --replace                  replace a database registered under <new-db> in the target databases.txt: its
                                 volumes, logs, _vinf, _dwb and line are removed first (refused while its server is up);
                                 as copydb -r, a failed migrate then leaves neither the old database nor a new one
  --source-databases-txt <file>  default: $CUBRID_DATABASES/databases.txt
  --target-databases-txt <file>  default: $CUBRID_DATABASES/databases.txt
  --source-conf <file>           the source server's cubrid.conf: ignore_trailing_space=yes there turns on the
                                 foreign key and LIST/RANGE compare checks (10.2 always has them)
  --target-conf <file>           the target server's cubrid.conf (default: $CUBRID/conf/cubrid.conf when it exists):
                                 ignore_trailing_space=yes refuses; db_volume_size, data_buffer_size and
                                 temp_file_max_size_in_pages give the defaults below
  --db-volume-size <size>        size cap of a volume the run adds when the target runs out of space (default: the
                                 target conf's db_volume_size, else 512M)
  --no-clone                     plan byte copies even where the filesystem could clone (clone is chosen per volume
                                 when its target directory is on the source volume's filesystem; use this when the
                                 target must not share blocks with the source)
  --volumes-in-first-dir         find volumes 1.. next to volume 0 (a source copied without its headers rewritten)
  -t, --thread-count <n>         worker limit the resource table assumes (default: CPUs / 2)
  --temp-mem-size <size>         memory limit the resource table assumes (default: the target conf's
                                 data_buffer_size, else physical memory / 4, at least 256M)
  --temp-disk-size min|max|<size>  temp disk limit (default: from the target conf's temp_file_max_size_in_pages:
                                 > 0 pages × page size, 0 min; else asked on a terminal, max otherwise)
  --check                        also count the temp disk the check needs on the source (migrate --check; one more
                                 read of every indexed heap)
  --json                         print plan.json instead of the table
The resource table (temp disk and memory per --temp-mem-size, docs/resources.md) needs one read of every indexed heap.";

/// Flags `plan` and `migrate` share.
pub const FLAGS: &[&str] = &[
    "--file-path",
    "--target-version",
    "--source-databases-txt",
    "--target-databases-txt",
    "--source-conf",
    "--target-conf",
    "--db-volume-size",
    "--temp-disk-size",
    "--extended-volume-path",
    "--control-file",
];
pub const SWITCHES: &[&str] = &["--no-clone", "--volumes-in-first-dir", "--replace"];

/// The choices `plan` and `migrate` share.
pub struct PlanOptions {
    pub db: String,
    pub target_dir: PathBuf,
    pub target_line: VersionLine,
    pub target_name: String,
    pub source_dbtxt: PathBuf,
    pub target_dbtxt: PathBuf,
    pub allow_clone: bool,
    /// The source server's `cubrid.conf`, read for `ignore_trailing_space` (workspace#541); none: not set.
    pub source_conf: Option<PathBuf>,
    /// The target server's `cubrid.conf` (default `$CUBRID/conf/cubrid.conf` when it exists).
    pub target_conf: Option<PathBuf>,
    /// The defaults that conf gives (workspace#604).
    pub conf: TargetConfDefaults,
    pub locate: Locate,
    /// `-r, --replace`: a database registered under the target name is replaced (workspace#604).
    pub replace: bool,
    /// `-E` or `-i` (workspace#604).
    pub layout: VolumeLayout,
    /// The target server's `db_volume_size` in bytes: size cap of an added volume (ADR 0013).
    pub db_volume_size: u64,
    /// `--temp-disk-size` as given; the conf's default needs the page size (`temp_disk_choice`).
    temp_disk: Option<TempDiskChoice>,
}

pub fn absolute(p: &str) -> Result<PathBuf, CliError> {
    std::path::absolute(p).map_err(CliError::Failed)
}

impl PlanOptions {
    pub fn from(p: &Parsed, what: &str) -> Result<Self, CliError> {
        let (db, new_db) = match p.positional.as_slice() {
            [db] => (db, db),
            [db, new_db] => (db, new_db),
            [] => {
                return Err(CliError::Usage(format!(
                    "{what} needs the source database name <db>"
                )));
            }
            _ => {
                return Err(CliError::Usage(format!(
                    "{what} takes <db> [<new-db>], not {} names",
                    p.positional.len()
                )));
            }
        };
        let default_dbtxt = || -> Result<PathBuf, CliError> {
            let dir = std::env::var_os("CUBRID_DATABASES").ok_or_else(|| {
                CliError::Usage("CUBRID_DATABASES is not set; pass the databases.txt path".into())
            })?;
            Ok(PathBuf::from(dir).join("databases.txt"))
        };
        let target_dir = absolute(
            p.value("--file-path")
                .ok_or_else(|| CliError::Usage("-F, --file-path is required".into()))?,
        )?;
        let target_line = match p.value("--target-version") {
            Some(v) => {
                parse_line(v).ok_or_else(|| CliError::Usage(format!("bad --target-version {v}")))?
            }
            None => target_install_version::from_install()?,
        };
        let target_conf = p.value("--target-conf").map(PathBuf::from).or_else(|| {
            std::env::var_os("CUBRID")
                .map(|c| PathBuf::from(c).join("conf/cubrid.conf"))
                .filter(|c| c.is_file())
        });
        let conf = TargetConfDefaults::read(target_conf.as_deref(), new_db)?;
        Ok(PlanOptions {
            db: db.clone(),
            target_dir,
            target_line,
            target_name: new_db.clone(),
            source_dbtxt: p
                .value("--source-databases-txt")
                .map(PathBuf::from)
                .map_or_else(default_dbtxt, Ok)?,
            target_dbtxt: p
                .value("--target-databases-txt")
                .map(PathBuf::from)
                .map_or_else(default_dbtxt, Ok)?,
            allow_clone: !p.switch("--no-clone"),
            source_conf: p.value("--source-conf").map(PathBuf::from),
            target_conf,
            locate: if p.switch("--volumes-in-first-dir") {
                Locate::FirstVolumeDir
            } else {
                Locate::Recorded
            },
            replace: p.switch("--replace"),
            layout: VolumeLayout::from(p)?,
            db_volume_size: match p.value("--db-volume-size") {
                Some(v) => limits::parse_size(v)
                    .ok_or_else(|| CliError::Usage(format!("bad --db-volume-size {v}")))?,
                // the target conf's db_volume_size, else the engine default (workspace#604)
                None => conf.db_volume_size.unwrap_or(512 << 20),
            },
            temp_disk: limits::temp_disk_choice(p)?,
            conf,
        })
    }

    /// `--temp-disk-size`, else what the target conf's `temp_file_max_size_in_pages` gives in pages of `page_size`.
    pub fn temp_disk_choice(&self, page_size: u64) -> Option<TempDiskChoice> {
        self.temp_disk.or_else(|| self.conf.temp_disk(page_size))
    }

    /// The `settings` object of plan.json: the values chosen from the command line, the target conf or the defaults.
    pub fn settings_json(&self, limits: &Limits, worker_limit: usize, page_size: u64) -> String {
        let temp_disk = match self.temp_disk_choice(page_size) {
            None => "null".to_string(),
            Some(TempDiskChoice::Min) => "\"min\"".to_string(),
            Some(TempDiskChoice::Max) => "\"max\"".to_string(),
            Some(TempDiskChoice::Bytes(b)) => b.to_string(),
        };
        format!(
            "{{\"target_name\": {}, \"target_version\": \"{}\", \"target_conf\": {}, \"db_volume_size\": {}, \"temp_mem_size\": {}, \"temp_disk_size\": {temp_disk}, \"thread_count\": {worker_limit}, \"replace\": {}}}",
            fastmig_verify::report::json_str(&self.target_name),
            line_name(self.target_line),
            self.target_conf
                .as_deref()
                .map_or("null".into(), |c| fastmig_verify::report::json_str(
                    &c.to_string_lossy()
                )),
            self.db_volume_size,
            limits.memory_limit_bytes,
            self.replace
        )
    }
}

/// The source as `plan` and `migrate` read it. The active log stays read-locked while this lives: a source whose
/// server is up is refused at open, and the log is read only through `lgat` (closing any other descriptor of the
/// file would drop the lock, workspace#494).
pub struct OpenSource {
    pub entry: DatabaseEntry,
    /// Held only for its read lock.
    _locked_log: fs::File,
    pub lgat_path: PathBuf,
    /// The release that wrote the source (from the log header, workspace#522).
    pub version: SourceVersion,
    pub log: LogHeader,
    pub vols: VolumeSet,
}

impl OpenSource {
    pub fn open(o: &PlanOptions) -> io::Result<Self> {
        let entry = databases_txt::lookup(&o.source_dbtxt, &o.db)?;
        let lgat_path = entry.log_path.join(format!("{}_lgat", o.db));
        let lgat = open_locked_log(&lgat_path)?;
        let (version, log) = read_log_header(&lgat, &lgat_path)?;
        let vols =
            VolumeSet::open_entry(&entry, o.locate, Access::ReadOnly, version.header_layout())?;
        Ok(OpenSource {
            entry,
            _locked_log: lgat,
            lgat_path,
            version,
            log,
            vols,
        })
    }

    /// Every source volume path (the storage kind of the source is judged by all of them, workspace#561).
    pub fn volume_paths(&self) -> Vec<PathBuf> {
        self.vols.volumes().iter().map(|v| v.path.clone()).collect()
    }
}

/// Reads the header, in the layout of the release that wrote it, through the locked handle `f` (the header lies in
/// the first 512 bytes of the log page).
pub fn read_log_header(f: &fs::File, path: &Path) -> io::Result<(SourceVersion, LogHeader)> {
    let mut page = vec![0u8; 512];
    std::os::unix::fs::FileExt::read_exact_at(f, &mut page, 0)?;
    log_header::read_source(&page)
        .map_err(|e| io::Error::other(format!("{}: {} at {}", path.display(), e.what, e.at)))
}

/// A usage error found while the plan is built (the `-i` control file names a volume the source does not have).
#[derive(Debug)]
pub struct UsageInPlan(String);

impl std::fmt::Display for UsageInPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UsageInPlan {}

/// A `make_plan` error as the command returns it: [`UsageInPlan`] is a usage error (exit 2).
pub fn plan_error(e: io::Error) -> CliError {
    match e.get_ref().and_then(|r| r.downcast_ref::<UsageInPlan>()) {
        Some(u) => CliError::Usage(u.0.clone()),
        None => CliError::Failed(e),
    }
}

/// The plan with the target-side checks the plan crate cannot see (the target name already registered, or the
/// database `--replace` removes and why it cannot), and that database.
pub fn make_plan(o: &PlanOptions, s: &OpenSource) -> io::Result<(Plan, Option<ReplacedDatabase>)> {
    let source: Vec<(i16, PathBuf)> = s
        .vols
        .volumes()
        .iter()
        .map(|v| (v.volid, v.path.clone()))
        .collect();
    let volume_targets = volume_targets::targets(&o.layout, &o.target_dir, &o.target_name, &source)
        .map_err(|e| match e {
            CliError::Usage(m) => io::Error::other(UsageInPlan(m)),
            CliError::Failed(e) => e,
        })?;
    let mut plan = fastmig_plan::build(&PlanInputs {
        source: &s.entry,
        source_dbtxt: &o.source_dbtxt,
        vols: &s.vols,
        log: &s.log,
        target_line: o.target_line,
        target_dir: &o.target_dir,
        volume_targets: &volume_targets,
        allow_clone: o.allow_clone,
        source_ignores_trailing_space: o
            .source_conf
            .as_deref()
            .map(|c| ignore_trailing_space(c, &o.db))
            .transpose()?
            .unwrap_or(false),
        target_ignores_trailing_space: o
            .target_conf
            .as_deref()
            .map(|c| ignore_trailing_space(c, &o.target_name))
            .transpose()?
            .unwrap_or(false),
    })?;
    let mut replaced = None;
    if o.replace {
        let (r, refusals) = replace_target::find_replaced(
            &o.target_dbtxt,
            &o.target_name,
            &s.volume_paths(),
            &o.target_dir,
            &volume_targets,
        )?;
        if let Some(r) = &r {
            plan.warnings.push(format!(
                "--replace: {} registered in {} is removed before the migration writes ({} volumes, {} log files)",
                o.target_name,
                o.target_dbtxt.display(),
                r.volumes.len(),
                r.logs.len()
            ));
        }
        plan.refusals.extend(refusals);
        replaced = r;
    } else {
        if let Err(e) = register_db::check_name_free(&o.target_dbtxt, &o.target_name) {
            plan.refusals
                .push(format!("{e} (pass -r, --replace to replace it)"));
        }
        let marker = o.target_dir.join(incomplete_marker::MARKER_NAME);
        for t in volume_targets
            .iter()
            .filter(|t| !t.starts_with(&o.target_dir))
        {
            if t.exists() && !marker.exists() {
                plan.refusals
                    .push(format!("target volume {} already exists", t.display()));
            }
        }
    }
    // a copied source still naming the original volumes (workspace#515)
    let (warnings, refusals) = volume_dirs::check(s, o.locate);
    plan.warnings.extend(warnings);
    plan.refusals.extend(refusals);
    // a rerun removes what the previous run left before it writes: that space is free to it (workspace#526)
    plan.available_bytes += incomplete_marker::reclaimable_bytes(&o.target_dir)?;
    Ok((plan, replaced))
}

pub fn run(argv: &[String]) -> Result<u8, CliError> {
    let flags = [FLAGS, limits::FLAGS, &["--thread-count"]].concat();
    let p = parse(argv, &flags, &[SWITCHES, &["--json", "--check"]].concat())?;
    let o = PlanOptions::from(&p, "plan")?;
    let s = OpenSource::open(&o)?;
    let plan = match make_plan(&o, &s) {
        // a source version with no codec line yet: its refusals, exit 1 like any refused plan
        Err(e) if e.get_ref().is_some_and(|r| r.is::<fastmig_plan::Refused>()) => {
            println!("{e}");
            return Ok(1);
        }
        Err(e) => return Err(plan_error(e)),
        Ok((plan, _)) => plan,
    };
    let limits = Limits::from(&p, &o.target_dir, o.conf.data_buffer_size)?;
    let mem = limits.memory_limit_bytes;
    let workers = limits::worker_limit(&p)?;
    let settings = o.settings_json(&limits, workers, s.vols.io_pagesize() as u64);
    let storage = RunStorage::detect(&s.volume_paths(), &o.target_dir, &limits.spill_dir);
    let est = match resource_estimate::estimate(
        &s.vols,
        plan.version_pair,
        plan.source_version,
        lob_base_dir(&s.entry).map_err(io::Error::other)?.as_deref(),
        workers,
        storage.source_extent_reads(),
        limits::physical_memory()?,
    ) {
        // the estimate encodes what the run writes, so a class the plan refused (a 10.2 stream with F_BENCHMARK,
        // workspace#540; an LDML collation key, workspace#491) can stop it too: the rest of the plan is still
        // printed, the estimate and the target space with why they are missing, exit 1 like migrate (workspace#584)
        Err(e) if !plan.refusals.is_empty() => Err(e.to_string()),
        r => Ok(r?),
    };
    let mut plan = plan;
    let space = match est {
        Ok(mut est) => {
            est.source_groups = check::source_key_groups(
                &s,
                o.locate,
                est.workers,
                mem,
                storage.source_extent_reads(),
            )?;
            if p.switch("--check") {
                let (bytes, pages) = check::source_pair_bytes(&s, o.locate, est.workers)?;
                est.verify_source = Some(bytes);
                est.verify_heap_pages = Some(pages);
            }
            let t = target_space::bound(&plan, &est, &s.vols, new_log::file_bytes(&s.log))?;
            if t.needed() > plan.available_bytes {
                plan.refusals.push(format!(
                    "the target filesystem has {} B free, the migration needs {} B before any temp disk",
                    plan.available_bytes,
                    t.needed()
                ));
            }
            Ok((t, est))
        }
        Err(e) => Err(e),
    };
    let storage_json = storage.json();
    match (&space, p.switch("--json")) {
        (Ok((t, est)), true) => print!(
            "{}",
            plan_report::json(
                &plan,
                Ok((t, est.json(mem).as_str())),
                &storage_json,
                &settings
            )
        ),
        (Ok((t, est)), false) => {
            print!("{}", plan_report::table(&plan, Ok(t)));
            print!("{}", storage.table());
            print!("{}", est.table(mem));
        }
        (Err(e), true) => print!(
            "{}",
            plan_report::json(&plan, Err(e.as_str()), &storage_json, &settings)
        ),
        (Err(e), false) => {
            print!("{}", plan_report::table(&plan, Err(e.as_str())));
            print!("{}", storage.table());
        }
    }
    Ok(u8::from(!plan.refusals.is_empty()))
}
