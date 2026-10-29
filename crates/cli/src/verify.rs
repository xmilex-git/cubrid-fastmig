//! `cubrid-fastmig verify`: runs the verifier on a database, or on a migrated target and its source together.
//!
//! Input: the database (a name in databases.txt, or the path of its first volume: then the active log is
//! `<path>_lgat`, the lob-base-path `<dir>/lob`, volumes 1.. next to it — the form of a T1 pristine copy), the
//! optional source, the worker and memory limits, the classes whose heap header statistics were measured.
//! Output: the JSONL report (`--report`, default `<target volume dir>/fastmig/verify.jsonl`), the summary line on
//! stdout; exit code 0 = no migration defect, 1 = migration defects, 2 = usage or tool error.
//! Source: G1 workspace#481 (verify command; finalize reads the exit code and the report), G3 workspace#483 Q9;
//! G2 workspace#482 + workspace#495 (`--measured-classes`: one class OID `volid|pageid|slotid` per line, written by
//! the migration until plan.json carries it).

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

use fastmig_index::{MemoryBudget, SpillDir, WorkerBudget};
use fastmig_ondisk::Oid;
use fastmig_ondisk::databases_txt;
use fastmig_ondisk::volume_set::Locate;
use fastmig_verify::report::{defect_line, exit_code, summary_line};
use fastmig_verify::{Database, Role, classify, verify_database};

use crate::args::{CliError, parse};
use crate::limits::{self, Limits};

const HELP: &str = "\
cubrid-fastmig verify <db | first-volume-path> [options]
  --source <db | path>           also verify the source; target defects the source has are pre-existing
  --role source|target           rules for <db> (default target: vacuum residue is a defect); source
                                 compares two engine-built databases of one line
  --databases-txt <file>         where <db> is registered (default: $CUBRID_DATABASES/databases.txt)
  --source-databases-txt <file>  where the source is registered (default: --databases-txt)
  --report <file>                JSONL report (default: <target volume dir>/fastmig/verify.jsonl)
  --measured-classes <file>      classes whose heap header statistics are compared (volid|pageid|slotid lines)
  --workers <n>                  thread cap (default: CPUs / 2)
  --mem <size>                   memory cap, e.g. 16G (default: physical memory / 4)
  --spill-dir <dir>              where sorted runs spill (default: <target volume dir>/fastmig/spill)";

const FLAGS: &[&str] = &[
    "--source",
    "--role",
    "--databases-txt",
    "--source-databases-txt",
    "--report",
    "--measured-classes",
    "--workers",
];

/// Where one database is and how to read it.
struct DbArg {
    first_volume: PathBuf,
    locate: Locate,
    log: PathBuf,
    lob_base: Option<PathBuf>,
}

fn resolve(arg: &str, dbtxt: Option<&str>) -> Result<DbArg, CliError> {
    if arg.contains('/') {
        let first = std::path::absolute(arg)?;
        let dir = first.parent().unwrap_or(Path::new("/")).to_path_buf();
        let lob = dir.join("lob");
        return Ok(DbArg {
            log: PathBuf::from(format!("{}_lgat", first.display())),
            first_volume: first,
            locate: Locate::FirstVolumeDir,
            lob_base: lob.is_dir().then_some(lob),
        });
    }
    let path = match dbtxt {
        Some(p) => PathBuf::from(p),
        None => PathBuf::from(std::env::var_os("CUBRID_DATABASES").ok_or_else(|| {
            CliError::Usage("CUBRID_DATABASES is not set; pass --databases-txt".into())
        })?)
        .join("databases.txt"),
    };
    let e = databases_txt::lookup(&path, arg)?;
    let lob_base = match &e.lob_base_path {
        Some(l) => Some(PathBuf::from(l.strip_prefix("file:").ok_or_else(|| {
            CliError::Failed(io::Error::other(format!("lob-base-path {l} is not file:")))
        })?)),
        None => None,
    };
    Ok(DbArg {
        first_volume: e.first_volume(),
        locate: Locate::Recorded,
        log: e.log_path.join(format!("{arg}_lgat")),
        lob_base,
    })
}

fn open(a: &DbArg, role: Role) -> io::Result<Database> {
    Database::open(&a.first_volume, a.locate, &a.log, role, a.lob_base.clone())
}

fn read_measured(path: &str) -> io::Result<BTreeSet<Oid>> {
    let text = std::fs::read_to_string(path)?;
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let f: Vec<&str> = l.trim().split('|').collect();
            match f.as_slice() {
                [v, p, s] => Ok(Oid::new(
                    v.parse().map_err(|_| bad(l))?,
                    p.parse().map_err(|_| bad(l))?,
                    s.parse().map_err(|_| bad(l))?,
                )),
                _ => Err(bad(l)),
            }
        })
        .collect()
}

fn bad(line: &str) -> io::Error {
    io::Error::other(format!("--measured-classes: bad line {line:?}"))
}

/// The exit code on success (0 or 1); usage and tool errors are `Err` (exit 2).
pub fn run(argv: &[String]) -> Result<u8, CliError> {
    let flags = [FLAGS, limits::FLAGS].concat();
    let p = parse(argv, &flags, &["--help"])?;
    if p.switch("--help") {
        println!("{HELP}");
        return Ok(0);
    }
    let [db] = p.positional.as_slice() else {
        return Err(CliError::Usage("verify takes exactly one database".into()));
    };
    let target = resolve(db, p.value("--databases-txt"))?;
    let source = p
        .value("--source")
        .map(|s| {
            resolve(
                s,
                p.value("--source-databases-txt")
                    .or(p.value("--databases-txt")),
            )
        })
        .transpose()?;
    let role = match p.value("--role") {
        None | Some("target") => Role::Target,
        Some("source") => Role::Source,
        Some(r) => return Err(CliError::Usage(format!("bad --role {r}"))),
    };
    let worker_limit = match p.value("--workers") {
        Some(w) => w
            .parse()
            .ok()
            .filter(|&n| n > 0)
            .ok_or_else(|| CliError::Usage(format!("bad --workers {w}")))?,
        None => std::thread::available_parallelism().map_or(1, |n| (n.get() / 2).max(1)),
    };
    let tdir = target
        .first_volume
        .parent()
        .unwrap_or(Path::new("/"))
        .to_path_buf();
    let lim = Limits::from(&p, &tdir)?;
    let report = p
        .value("--report")
        .map_or_else(|| tdir.join("fastmig").join("verify.jsonl"), PathBuf::from);
    let measured = p
        .value("--measured-classes")
        .map(read_measured)
        .transpose()?
        .unwrap_or_default();

    let wb = WorkerBudget::new(worker_limit);
    let mb = MemoryBudget::new(usize::try_from(lim.memory_limit_bytes).unwrap_or(usize::MAX));
    let spill = SpillDir::create(&lim.spill_dir)?;
    let limits = fastmig_verify::Limits {
        workers: &wb,
        memory: &mb,
        spill: &spill,
    };
    let tdb = open(&target, role)?;
    let tres = verify_database(&tdb, &measured, &limits)?;
    drop(tdb);
    let sres = match &source {
        Some(s) => {
            let sdb = open(s, Role::Source)?;
            Some(verify_database(&sdb, &BTreeSet::new(), &limits)?)
        }
        None => None,
    };
    let all = classify(
        tres.defects.clone(),
        sres.as_ref().map(|r| r.defects.clone()),
    );
    let summary = summary_line(&all, &tres, sres.as_ref(), worker_limit);
    let mut text = String::new();
    for c in &all {
        text.push_str(&defect_line(c));
        text.push('\n');
    }
    text.push_str(&summary);
    text.push('\n');
    if let Some(dir) = report.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&report, text)?;
    println!("{summary}");
    Ok(exit_code(&all))
}
