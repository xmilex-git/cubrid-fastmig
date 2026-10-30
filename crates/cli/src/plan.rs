//! `cubrid-fastmig plan`: reads the source only and prints what `migrate` would do; also the source opening and the
//! plan building `migrate` shares, so both decide the same way.
//!
//! Input: the source DB name (found in the source `databases.txt`), the target directory and the choices
//! (`--target-version`, `--target-name`, `--no-clone`). Output: the plan as a table on stdout, or JSON with
//! `--json`; exit code 1 when the plan has refusals.
//! Source: decision G1 workspace#481 (plan writes nothing; refusals and warnings), workspace#505 (`--target-version`
//! decided once by the plan).

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
use fastmig_plan::version_pair::parse_line;
use fastmig_plan::{Plan, PlanInputs};

use crate::args::{CliError, Parsed, parse};
use crate::limits::{self, Limits};
use crate::{plan_report, resource_estimate, target_space, verify, volume_dirs};

const HELP: &str = "\
cubrid-fastmig plan <db> --dest <dir> [options]
  --dest <dir>                   target directory migrate would write
  --target-version 11.4|develop  target release line (default: develop; 11.4 is refused: no 11.4 layout
                                 B-tree builder)
  --target-name <name>           target DB name (default: <db>)
  --source-databases-txt <file>  default: $CUBRID_DATABASES/databases.txt
  --target-databases-txt <file>  default: $CUBRID_DATABASES/databases.txt
  --source-conf <file>           the source server's cubrid.conf: ignore_trailing_space=yes there turns on the
                                 foreign key and LIST/RANGE compare checks (10.2 always has them)
  --target-conf <file>           the target server's cubrid.conf; ignore_trailing_space=yes refuses (default:
                                 $CUBRID/conf/cubrid.conf when it exists)
  --no-clone                     plan byte copies even where the filesystem could clone
  --volumes-in-first-dir         find volumes 1.. next to volume 0 (a source copied without its headers rewritten)
  --workers <n>                  worker limit the resource table assumes (default: CPUs / 2)
  --mem <size>                   memory limit the resource table assumes (default: physical memory / 4, at least 256M)
  --verify                       also count the temp disk the verifier needs on the source (migrate --verify;
                                 one more read of every indexed heap)
  --json                         print plan.json instead of the table
The resource table (temp disk and memory per --mem, docs/resources.md) needs one read of every indexed heap.";

/// Flags `plan` and `migrate` share.
pub const FLAGS: &[&str] = &[
    "--dest",
    "--target-version",
    "--target-name",
    "--source-databases-txt",
    "--target-databases-txt",
    "--source-conf",
    "--target-conf",
];
pub const SWITCHES: &[&str] = &["--no-clone", "--volumes-in-first-dir"];

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
    pub locate: Locate,
}

pub fn absolute(p: &str) -> Result<PathBuf, CliError> {
    std::path::absolute(p).map_err(CliError::Failed)
}

impl PlanOptions {
    pub fn from(p: &Parsed, what: &str) -> Result<Self, CliError> {
        let [db] = p.positional.as_slice() else {
            return Err(CliError::Usage(format!(
                "{what} takes exactly one database name"
            )));
        };
        let default_dbtxt = || -> Result<PathBuf, CliError> {
            let dir = std::env::var_os("CUBRID_DATABASES").ok_or_else(|| {
                CliError::Usage("CUBRID_DATABASES is not set; pass the databases.txt path".into())
            })?;
            Ok(PathBuf::from(dir).join("databases.txt"))
        };
        let target_version = p.value("--target-version").unwrap_or("develop");
        Ok(PlanOptions {
            db: db.clone(),
            target_dir: absolute(
                p.value("--dest")
                    .ok_or_else(|| CliError::Usage("--dest is required".into()))?,
            )?,
            target_line: parse_line(target_version)
                .ok_or_else(|| CliError::Usage(format!("bad --target-version {target_version}")))?,
            target_name: p.value("--target-name").unwrap_or(db).to_string(),
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
            target_conf: p.value("--target-conf").map(PathBuf::from).or_else(|| {
                std::env::var_os("CUBRID")
                    .map(|c| PathBuf::from(c).join("conf/cubrid.conf"))
                    .filter(|c| c.is_file())
            }),
            locate: if p.switch("--volumes-in-first-dir") {
                Locate::FirstVolumeDir
            } else {
                Locate::Recorded
            },
        })
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
}

/// Reads the header, in the layout of the release that wrote it, through the locked handle `f` (the header lies in
/// the first 512 bytes of the log page).
pub fn read_log_header(f: &fs::File, path: &Path) -> io::Result<(SourceVersion, LogHeader)> {
    let mut page = vec![0u8; 512];
    std::os::unix::fs::FileExt::read_exact_at(f, &mut page, 0)?;
    log_header::read_source(&page)
        .map_err(|e| io::Error::other(format!("{}: {} at {}", path.display(), e.what, e.at)))
}

/// The plan, with the target-side refusal the plan crate cannot see (the target name already registered).
pub fn make_plan(o: &PlanOptions, s: &OpenSource) -> io::Result<Plan> {
    let mut plan = fastmig_plan::build(&PlanInputs {
        source: &s.entry,
        source_dbtxt: &o.source_dbtxt,
        vols: &s.vols,
        log: &s.log,
        target_line: o.target_line,
        target_dir: &o.target_dir,
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
    if let Err(e) = register_db::check_name_free(&o.target_dbtxt, &o.target_name) {
        plan.refusals.push(e.to_string());
    }
    // a copied source still naming the original volumes (workspace#515)
    let (warnings, refusals) = volume_dirs::check(s, o.locate);
    plan.warnings.extend(warnings);
    plan.refusals.extend(refusals);
    // a rerun removes what the previous run left before it writes: that space is free to it (workspace#526)
    plan.available_bytes += incomplete_marker::reclaimable_bytes(&o.target_dir)?;
    Ok(plan)
}

pub fn run(argv: &[String]) -> Result<u8, CliError> {
    let flags = [FLAGS, limits::FLAGS, &["--workers"]].concat();
    let p = parse(
        argv,
        &flags,
        &[SWITCHES, &["--json", "--verify", "--help"]].concat(),
    )?;
    if p.switch("--help") {
        println!("{HELP}");
        return Ok(0);
    }
    let o = PlanOptions::from(&p, "plan")?;
    let s = OpenSource::open(&o)?;
    let plan = match make_plan(&o, &s) {
        // a source version with no codec line yet: its refusals, exit 1 like any refused plan
        Err(e) if e.get_ref().is_some_and(|r| r.is::<fastmig_plan::Refused>()) => {
            println!("{e}");
            return Ok(1);
        }
        r => r?,
    };
    let mem = Limits::from(&p, &o.target_dir)?.memory_limit_bytes;
    let mut est = match resource_estimate::estimate(
        &s.vols,
        plan.version_pair,
        plan.source_version,
        lob_base_dir(&s.entry).map_err(io::Error::other)?.as_deref(),
        limits::worker_limit(&p)?,
        limits::physical_memory()?,
    ) {
        // the estimate encodes what the run writes, so a class the plan refused (a 10.2 stream with F_BENCHMARK,
        // workspace#540) can stop it too: the refusals are the answer, exit 1 like migrate
        Err(e) if !plan.refusals.is_empty() => {
            println!(
                "the plan refuses the migration:\n  {}\n(no resource estimate: {e})",
                plan.refusals.join("\n  ")
            );
            return Ok(1);
        }
        r => r?,
    };
    est.source_groups = verify::source_key_groups(&s, o.locate, est.workers)?;
    if p.switch("--verify") {
        let (bytes, pages) = verify::source_pair_bytes(&s, o.locate, est.workers)?;
        est.verify_source = Some(bytes);
        est.verify_heap_pages = Some(pages);
    }
    let t = target_space::bound(&plan, &est, &s.vols, new_log::file_bytes(&s.log))?;
    let mut plan = plan;
    if t.needed() > plan.available_bytes {
        plan.refusals.push(format!(
            "the target filesystem has {} B free, the migration needs {} B before any temp disk",
            plan.available_bytes,
            t.needed()
        ));
    }
    if p.switch("--json") {
        print!("{}", plan_report::json(&plan, &t, &est.json(mem)));
    } else {
        print!("{}", plan_report::table(&plan, &t));
        print!("{}", est.table(mem));
    }
    Ok(u8::from(!plan.refusals.is_empty()))
}
