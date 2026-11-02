//! `cubrid-fastmig post` and the end of `migrate --run-sql`/`--engine-check`: on a registered target, the target
//! install's engine in SA mode runs the required SQL and, with `--engine-check`, checks the target against the tool;
//! the outcome joins report.json as its `post` item and the run log.
//!
//! Input: the registered target (name, volume 0, active log, databases.txt, `<dest>/fastmig/`), the required SQL
//! files in their order, the install `$CUBRID` of the target's line, the options. Output: [`PostOutcome`]; its
//! `post` item spliced into report.json (the other keys unchanged), its lines appended to `migrate.log`, the
//! engine's outputs in `<dest>/fastmig/post/`. Exit of the command: 0 = all passed, 3 = an SQL file or a check failed
//! (the target stays registered), 2 = usage or refused before an engine ran (or the source copy did not fit).
//! Order: the tool counts the target's rows before any engine process opens it; then the SQL files; then, only when
//! they all ran, the engine check (row counts, index checks), `checkdb` with `--engine-checkdb`, the data comparison
//! with `--source-cubrid`. `post-migration-fixes.sql` is never run (decision 4).
//! Source: workspace#611 decisions 1-5, ADR 0018.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use fastmig_copy::incomplete_marker::{CREATED_PATHS, MARKER_NAME};
use fastmig_ondisk::databases_txt;
use fastmig_verify::report::json_str;

use crate::args::{CliError, parse};
use crate::engine_check::{self, EngineCheck, IndexResult};
use crate::engine_sa::EngineSa;
use crate::required_sql_run::{self, SqlFileRun};
use crate::source_fingerprint::{self, SourceFingerprint};
use crate::unload_comparison::{self, Comparison};
use crate::{class_live_rows, limits, target_install_version};

pub const HELP: &str = "\
cubrid-fastmig post <target-dir> [options]
  On a target migrate registered, runs the required SQL with the target install's csql in SA mode ($CUBRID, the
  target's release line), as migrate --run-sql does; with --engine-check also checks the target with the engine.
  post-migration-fixes.sql is not run: review it first. The outcome is the post item of <dest>/fastmig/report.json
  and the end of migrate.log; the engine output is in <dest>/fastmig/post/.
  --engine-check                 after the SQL: per class the engine's row count equals the tool's, per index a
                                 query through it counts what a heap scan counts (finalize refuses a target whose
                                 engine check failed)
  --engine-checkdb               also cubrid checkdb -S (turns on --engine-check)
  --source-cubrid <install>      also compare all data: the source's own install unloads a copy of the source, the
                                 target install unloads the target, set-comparison compares them (turns on
                                 --engine-check; the copy needs the source's size of free space unless cloned)
  --target-databases-txt <file>  the databases.txt that registers the target (default: $CUBRID_DATABASES/databases.txt)
  -t, --thread-count <n>         threads of the tool's row count (default: CPUs / 2)
  --temp-mem-size <size>         memory limit of the tool's row count (default: physical memory / 4)
  exit 0 = passed, 3 = an SQL file or a check failed, 2 = usage error or refused";

/// The post command's own work directory in `<dest>/fastmig/`.
pub const WORK_DIR: &str = "post";

pub struct PostOptions {
    pub engine_check: bool,
    pub checkdb: bool,
    pub source_install: Option<PathBuf>,
}

pub const FLAGS: &[&str] = &["--source-cubrid"];
pub const SWITCHES: &[&str] = &["--engine-check", "--engine-checkdb"];

impl PostOptions {
    pub fn from(p: &crate::args::Parsed) -> Result<Self, CliError> {
        let source_install = p
            .value("--source-cubrid")
            .map(std::path::absolute)
            .transpose()?;
        Ok(PostOptions {
            engine_check: p.switch("--engine-check")
                || p.switch("--engine-checkdb")
                || source_install.is_some(),
            checkdb: p.switch("--engine-checkdb"),
            source_install,
        })
    }
}

/// The install of the target's line (`$CUBRID`), checked before anything runs: the engine must be the target's.
pub fn target_install(target_line: &str) -> Result<PathBuf, CliError> {
    let line = target_install_version::from_install()?;
    let name = fastmig_plan::version_pair::line_name(line);
    if name != target_line {
        return Err(CliError::Usage(format!(
            "the install $CUBRID is the {name} line, the target is {target_line}: the engine check needs the target's line"
        )));
    }
    Ok(PathBuf::from(
        std::env::var_os("CUBRID").unwrap_or_default(),
    ))
}

pub struct PostTarget {
    pub name: String,
    pub first_volume: PathBuf,
    pub log: PathBuf,
    pub databases_txt: PathBuf,
    pub fastmig_dir: PathBuf,
}

pub struct PostOutcome {
    pub sql: Vec<SqlFileRun>,
    /// Required files that did not run after a failed one.
    pub not_run: Vec<String>,
    pub engine_check_ran: bool,
    pub engine: Option<EngineCheck>,
    /// checkdb: passed and its output file.
    pub checkdb: Option<(bool, PathBuf)>,
    pub comparison: Option<Comparison>,
    pub work_dir: PathBuf,
    /// post-migration-fixes.sql holds statements for the DBA.
    pub fixes_to_review: bool,
}

impl PostOutcome {
    fn sql_passed(&self) -> bool {
        self.not_run.is_empty() && self.sql.iter().all(|r| r.error.is_none())
    }

    pub fn refused(&self) -> bool {
        matches!(self.comparison, Some(Comparison::Refused(_)))
    }

    pub fn passed(&self) -> bool {
        self.sql_passed()
            && (!self.engine_check_ran || self.engine.as_ref().is_some_and(EngineCheck::passed))
            && self.checkdb.as_ref().is_none_or(|c| c.0)
            && self
                .comparison
                .as_ref()
                .is_none_or(|c| matches!(c, Comparison::Compared { same: true, .. }))
    }

    /// The exit code a run with this outcome ends with.
    pub fn exit_code(&self) -> u8 {
        if self.refused() {
            2
        } else if self.passed() {
            0
        } else {
            3
        }
    }

    /// The `post` item of report.json, on one line: finalize reads its head (`{"passed": .., "engine_check_ran": ..`).
    pub fn json(&self) -> String {
        let sql: Vec<String> = self
            .sql
            .iter()
            .map(|r| {
                let error = r.error.as_ref().map_or("null".into(), |e| {
                    format!(
                        "{{\"line\": {}, \"text\": {}, \"err\": {}}}",
                        e.line.map_or("null".into(), |n| n.to_string()),
                        json_str(&e.text),
                        r.err_file
                            .as_ref()
                            .map_or("null".into(), |p| json_str(&p.to_string_lossy()))
                    )
                });
                format!(
                    "{{\"file\": {}, \"seconds\": {:.3}, \"earlier\": {}, \"error\": {error}}}",
                    json_str(&r.file),
                    r.seconds,
                    r.earlier
                )
            })
            .collect();
        let not_run: Vec<String> = self.not_run.iter().map(|f| json_str(f)).collect();
        let engine = self.engine.as_ref().map_or("null".into(), |e| {
            let (p, f, s) = e.index_counts();
            let diffs: Vec<String> = e
                .class_differences
                .iter()
                .map(|c| {
                    let n = |v: Option<u64>| v.map_or("null".into(), |n| n.to_string());
                    format!("{{\"class\": {}, \"tool\": {}, \"engine\": {}}}", json_str(&c.class), n(c.tool), n(c.engine))
                })
                .collect();
            let skipped: Vec<String> = s.iter().map(|(r, n)| format!("{}: {n}", json_str(r))).collect();
            format!(
                "{{\"passed\": {}, \"classes_checked\": {}, \"class_differences\": [{}], \"indexes\": {{\"passed\": {p}, \"failed\": {f}, \"skipped\": {{{}}}}}, \"error\": {}}}",
                e.passed(),
                e.classes_checked,
                diffs.join(", "),
                skipped.join(", "),
                e.error.as_deref().map_or("null".into(), json_str)
            )
        });
        let checkdb = self.checkdb.as_ref().map_or("null".into(), |(ok, f)| {
            format!(
                "{{\"passed\": {ok}, \"output\": {}}}",
                json_str(&f.to_string_lossy())
            )
        });
        let comparison = self.comparison.as_ref().map_or("null".into(), |c| {
            let (result, detail) = match c {
                Comparison::Refused(d) => ("refused", d.as_str()),
                Comparison::Failed(d) => ("failed", d.as_str()),
                Comparison::Compared { same: true, output } => ("same", output.as_str()),
                Comparison::Compared {
                    same: false,
                    output,
                } => ("differ", output.as_str()),
            };
            format!(
                "{{\"result\": \"{result}\", \"detail\": {}}}",
                json_str(detail)
            )
        });
        format!(
            "{{\"passed\": {}, \"engine_check_ran\": {}, \"run_sql\": [{}], \"not_run\": [{}], \"engine_check\": {engine}, \"checkdb\": {checkdb}, \"data_comparison\": {comparison}, \"work_dir\": {}}}",
            self.passed(),
            self.engine_check_ran,
            sql.join(", "),
            not_run.join(", "),
            json_str(&self.work_dir.to_string_lossy())
        )
    }

    /// The outcome in a few lines: the end screen's SQL box and stderr.
    pub fn summary(&self) -> Vec<String> {
        let mut l = Vec::new();
        match self.sql.iter().find(|r| r.error.is_some()) {
            None => l.push(format!(
                "run-sql: {} ran without error ({})",
                if self.sql.len() == 1 {
                    "1 file".to_string()
                } else {
                    format!("{} files", self.sql.len())
                },
                self.sql
                    .iter()
                    .map(|r| match r.earlier {
                        true => format!("{} ran earlier", r.file),
                        false => format!("{} {:.1} s", r.file, r.seconds),
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            Some(r) => {
                let e = r.error.as_ref().unwrap();
                let at = e.line.map_or(String::new(), |n| format!(" line {n}"));
                l.push(format!("run-sql: FAILED in {}{at}: {}", r.file, e.text));
                if let Some(p) = &r.err_file {
                    l.push(format!("run-sql: csql errors in {}", p.display()));
                }
                if !self.not_run.is_empty() {
                    l.push(format!("run-sql: not run: {}", self.not_run.join(", ")));
                }
            }
        }
        if self.engine_check_ran {
            match &self.engine {
                None => l.push("engine check: not run (the required SQL failed)".into()),
                Some(e) => {
                    if let Some(err) = &e.error {
                        l.push(format!("engine check: FAILED: {err}"));
                    }
                    l.push(if e.class_differences.is_empty() {
                        format!(
                            "rows: {} classes, engine count = tool count",
                            e.classes_checked
                        )
                    } else {
                        format!(
                            "rows: FAILED, {} of {} classes differ (first: {})",
                            e.class_differences.len(),
                            e.classes_checked,
                            e.class_differences[0].class
                        )
                    });
                    let (p, f, s) = e.index_counts();
                    let skipped: usize = s.values().sum();
                    let reasons: Vec<String> = s.iter().map(|(r, n)| format!("{r}: {n}")).collect();
                    let reasons = if reasons.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", reasons.join("; "))
                    };
                    l.push(format!(
                        "index checks: {p} passed, {skipped} skipped{reasons}{}",
                        if f > 0 {
                            format!(", {f} FAILED")
                        } else {
                            String::new()
                        }
                    ));
                }
            }
        }
        if let Some((ok, f)) = &self.checkdb {
            l.push(format!(
                "checkdb: {} ({})",
                if *ok { "passed" } else { "FAILED" },
                f.display()
            ));
        }
        match &self.comparison {
            None => {}
            Some(Comparison::Refused(d)) => l.push(format!("data comparison: refused: {d}")),
            Some(Comparison::Failed(d)) => l.push(format!("data comparison: FAILED: {d}")),
            Some(Comparison::Compared { same, output }) => l.push(format!(
                "data comparison: {} ({output})",
                if *same { "same entries" } else { "DIFFER" }
            )),
        }
        if self.fixes_to_review {
            l.push("post-migration-fixes.sql: not run; review it, then run it as DBA".into());
        }
        l
    }

    /// The run log's lines: the summary, every skipped or failed index and every class whose count differs.
    pub fn log_lines(&self) -> Vec<String> {
        let mut l: Vec<String> = self
            .summary()
            .into_iter()
            .map(|s| format!("post: {s}"))
            .collect();
        if let Some(e) = &self.engine {
            for c in &e.class_differences {
                l.push(format!(
                    "post: rows differ: {} tool {:?} engine {:?}",
                    c.class, c.tool, c.engine
                ));
            }
            for i in &e.indexes {
                match &i.result {
                    IndexResult::Skipped(r) => {
                        l.push(format!("post: index skipped: {} ({r})", i.index))
                    }
                    IndexResult::Failed(r) => {
                        l.push(format!("post: index FAILED: {} ({r})", i.index))
                    }
                    IndexResult::Passed { predicate, rows } => l.push(format!(
                        "post: index passed: {} (key {predicate} its minimum, {rows} rows)",
                        i.index
                    )),
                }
            }
        }
        l.push(format!(
            "post: engine output in {}",
            self.work_dir.display()
        ));
        l
    }
}

/// Appends `path` to the target's created-paths (a later `--replace` removes it with the rest).
fn record_created(fastmig_dir: &Path, path: &Path) -> io::Result<()> {
    let created = fastmig_dir.join(CREATED_PATHS);
    let mut f = OpenOptions::new()
        .append(true)
        .create(true)
        .open(&created)?;
    f.write_all(format!("{}\n", path.display()).as_bytes())?;
    f.sync_all()
}

/// Runs the required SQL and the requested checks on `t` (registered, no incomplete marker).
#[allow(clippy::too_many_arguments)]
pub fn post(
    t: &PostTarget,
    o: &PostOptions,
    files: &[String],
    ran_before: &[String],
    install: &Path,
    pair: (&str, &str),
    workers: usize,
    memory_bytes: u64,
) -> io::Result<PostOutcome> {
    let work = t.fastmig_dir.join(WORK_DIR);
    if !work.exists() {
        record_created(&t.fastmig_dir, &work)?;
    }
    fs::create_dir_all(&work)?;
    unload_comparison::clear(&work)?;
    let tool_rows = if o.engine_check {
        let spill = work.join("spill");
        let rows = class_live_rows::count(&t.first_volume, &t.log, workers, memory_bytes, &spill);
        if spill.exists() {
            fs::remove_dir_all(&spill)?;
        }
        Some(rows?)
    } else {
        None
    };
    let engine = EngineSa::new(install, &t.databases_txt, &work)?;
    let sql = required_sql_run::run(&engine, &t.name, &t.fastmig_dir, files, ran_before)?;
    let not_run = files[sql.len()..].to_vec();
    let fixes = t.fastmig_dir.join("post-migration-fixes.sql");
    let mut out = PostOutcome {
        not_run,
        engine_check_ran: o.engine_check,
        engine: None,
        checkdb: None,
        comparison: None,
        work_dir: work.clone(),
        fixes_to_review: fs::metadata(&fixes).is_ok_and(|m| m.len() > 0),
        sql,
    };
    let Some(tool_rows) = tool_rows.filter(|_| out.sql_passed()) else {
        return Ok(out);
    };
    out.engine = Some(engine_check::run(&engine, &t.name, &tool_rows)?);
    if o.checkdb {
        let r = engine.run("checkdb", "cubrid", &["checkdb", "-S", &t.name])?;
        out.checkdb = Some((r.code == Some(0), r.log));
    }
    if let Some(src_install) = &o.source_install {
        let fp = SourceFingerprint::read(&t.fastmig_dir.join(source_fingerprint::FILE_NAME))?;
        let record = |p: &Path| record_created(&t.fastmig_dir, p);
        out.comparison = Some(unload_comparison::run(
            &fp,
            src_install,
            &engine,
            &t.name,
            pair,
            &work,
            &record,
        )?);
    }
    Ok(out)
}

/// Splices the `post` item into report.json (replacing an earlier one) and appends the log lines to migrate.log.
pub fn record(fastmig_dir: &Path, outcome: &PostOutcome) -> io::Result<()> {
    let path = fastmig_dir.join("report.json");
    let text = fs::read_to_string(&path)?;
    let body = text
        .trim_end()
        .strip_suffix('}')
        .unwrap_or(&text)
        .trim_end();
    let body = body.split_once(",\n  \"post\": ").map_or(body, |(b, _)| b);
    fs::write(
        &path,
        format!("{body},\n  \"post\": {}\n}}\n", outcome.json()),
    )?;
    let mut log = OpenOptions::new()
        .append(true)
        .create(true)
        .open(fastmig_dir.join(crate::migrate::RUN_LOG))?;
    for l in outcome.log_lines() {
        writeln!(log, "{l}")?;
    }
    Ok(())
}

/// The `post` item of report.json says an engine check ran and failed: finalize refuses (decision 5).
pub fn engine_check_failed(report_json: &str) -> bool {
    report_json.contains("\"post\": {\"passed\": false, \"engine_check_ran\": true")
}

/// `"key": "value"` of report.json's first object holding `key`.
fn json_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let at = text.find(&format!("\"{key}\": \""))? + key.len() + 5;
    text[at..].split('"').next()
}

/// The files the earlier post item of report.json ran without error (or skipped as ran earlier still): a rerun of
/// post does not run them again.
pub fn ran_earlier(report: &str) -> Vec<String> {
    let Some(at) = report.find("\"post\": {") else {
        return Vec::new();
    };
    let item = &report[at..];
    let item = &item[..item.find("\"not_run\"").unwrap_or(item.len())];
    item.split("{\"file\": \"")
        .skip(1)
        .filter(|e| e.contains("\"error\": null"))
        .filter_map(|e| e.split('"').next())
        .map(str::to_string)
        .collect()
}

/// The required SQL files report.json lists, in their order.
pub fn required_files(report: &str) -> Vec<String> {
    let Some(at) = report.find("\"required_sql\": [") else {
        return Vec::new();
    };
    let list = &report[at..];
    let list = &list[..list.find(']').unwrap_or(list.len())];
    list.match_indices("\"file\": \"")
        .filter_map(|(i, m)| list[i + m.len()..].split('"').next())
        .map(str::to_string)
        .collect()
}

/// The target registered with volume directory `dir` in `dbtxt`.
fn find_target(dbtxt: &Path, dir: &Path) -> io::Result<databases_txt::DatabaseEntry> {
    let text = fs::read_to_string(dbtxt)?;
    let name = text
        .lines()
        .map(|l| {
            l.split('#')
                .next()
                .unwrap_or("")
                .split_whitespace()
                .collect::<Vec<_>>()
        })
        .find(|f| f.len() >= 4 && Path::new(f[1]) == dir)
        .map(|f| f[0].to_string())
        .ok_or_else(|| {
            io::Error::other(format!(
                "no database of {} has the volume directory {}: post needs the registered target",
                dbtxt.display(),
                dir.display()
            ))
        })?;
    databases_txt::lookup(dbtxt, &name)
}

/// `cubrid-fastmig post`.
pub fn run(argv: &[String]) -> Result<u8, CliError> {
    let p = parse(
        argv,
        &[
            FLAGS,
            &[
                "--target-databases-txt",
                "--thread-count",
                "--temp-mem-size",
            ],
        ]
        .concat(),
        SWITCHES,
    )?;
    let [dir] = p.positional.as_slice() else {
        return Err(CliError::Usage("post takes the target directory".into()));
    };
    let dir = std::path::absolute(dir)?;
    let o = PostOptions::from(&p)?;
    let fastmig_dir = dir.join("fastmig");
    if dir.join(MARKER_NAME).exists() {
        return Err(CliError::Usage(format!(
            "{} holds {MARKER_NAME}: the migration did not finish",
            dir.display()
        )));
    }
    let report = fs::read_to_string(fastmig_dir.join("report.json"))?;
    let (Some(src_line), Some(tgt_line)) =
        (json_value(&report, "source"), json_value(&report, "target"))
    else {
        return Err(CliError::Failed(io::Error::other(
            "report.json names no version_pair",
        )));
    };
    let install = target_install(tgt_line)?;
    let dbtxt = match p.value("--target-databases-txt") {
        Some(f) => std::path::absolute(f)?,
        None => PathBuf::from(std::env::var_os("CUBRID_DATABASES").ok_or_else(|| {
            CliError::Usage("CUBRID_DATABASES is not set: pass --target-databases-txt".into())
        })?)
        .join("databases.txt"),
    };
    let entry = find_target(&dbtxt, &dir)?;
    let workers = limits::worker_limit(&p)?;
    let memory = match p.value("--temp-mem-size") {
        Some(v) => limits::parse_size(v)
            .ok_or_else(|| CliError::Usage(format!("--temp-mem-size {v}: not a size")))?,
        None => limits::physical_memory()? / 4,
    };
    let t = PostTarget {
        first_volume: entry.first_volume(),
        log: entry.log_path.join(format!("{}_lgat", entry.name)),
        name: entry.name,
        databases_txt: dbtxt,
        fastmig_dir: fastmig_dir.clone(),
    };
    let out = post(
        &t,
        &o,
        &required_files(&report),
        &ran_earlier(&report),
        &install,
        (src_line, tgt_line),
        workers,
        memory,
    )?;
    record(&fastmig_dir, &out)?;
    for l in out.summary() {
        println!("{l}");
    }
    Ok(out.exit_code())
}
