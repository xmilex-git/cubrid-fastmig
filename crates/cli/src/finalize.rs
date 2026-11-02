//! `cubrid-fastmig finalize`: deletes the source database's files once its migration verified and the source is
//! unchanged — the only command that deletes anything of the source.
//!
//! Input: the target directory of a finished migrate (`<dest>/fastmig/` holds the source fingerprint, the LOB
//! manifests and the check report `check.jsonl`). Output: the source's files removed, its databases.txt line removed, and
//! `<dest>/fastmig/finalize.json` listing what was deleted and the LOB files kept.
//! Guards: no incomplete marker in the target; a check report (`migrate --check` or a later `check` run, ADR 0015)
//! whose summary has 0 migration defects; the source
//! fingerprint (log header append_lsa and db_creation read under the read lock, every volume's size, every
//! referenced LOB file's size) equals the one migrate recorded.
//! Deleted: the recorded volumes; `<db>_lgat`, `<db>_lgar*`, `<db>_lginf`, `<db>_vinf`, `<db>_dwb` of the log
//! directory; the referenced LOB files, then `ces_*` directories and the lob-base directory left empty; `java` and
//! `java_static` of the volume directory. Kept: backups, `_bkvinf`, `_keys`, orphan LOB files and files outside the
//! lob-base-path (listed in finalize.json).
//! Also run by `migrate -d, --delete-source` at its end when the check found 0 migration defects (ADR 0008, G13
//! workspace#604).
//! Source: decision G1 workspace#481 (guard and delete list), G6 workspace#488 8-10 (LOB and Java files), ADR 0008.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use fastmig_copy::incomplete_marker::{CREATED_PATHS, MARKER_NAME};
use fastmig_copy::lob_manifest;
use fastmig_copy::open_locked_log;
use fastmig_verify::report::json_str;

use crate::args::{CliError, parse};
use crate::database_files::{log_files, remove_file, unregister};
use crate::plan::read_log_header;
use crate::source_fingerprint::{self, SourceFingerprint};

pub const HELP: &str = "\
cubrid-fastmig finalize <target-dir>
  Deletes the source of the migration in <target-dir> once its check found 0 migration defects and the source is
  unchanged (ADR 0008).";

/// `<dest>/fastmig/lob-outside-base`: LOB files the source names by an absolute path outside its lob-base-path
/// (`<size>\t<absolute path>` lines), kept by finalize.
pub const OUTSIDE_BASE: &str = "lob-outside-base";

/// `<dest>/fastmig/check.jsonl`: the check report migrate --check and check write, finalize reads.
pub const CHECK_REPORT: &str = "check.jsonl";
/// The standalone check's log beside its report: the summary line and the report path (workspace#610).
pub const CHECK_LOG: &str = "check.log";

fn refuse(msg: String) -> CliError {
    CliError::Failed(io::Error::other(format!("finalize refused: {msg}")))
}

/// The source as it is now, read the way migrate recorded it.
fn current(f: &SourceFingerprint) -> io::Result<SourceFingerprint> {
    // the read lock refuses a source whose server is up
    let lgat = open_locked_log(&f.log)?;
    let (_, lh) = read_log_header(&lgat, &f.log)?;
    let volumes = f
        .volumes
        .iter()
        .map(|(_, p)| Ok((fs::metadata(p)?.len(), p.clone())))
        .collect::<io::Result<_>>()?;
    Ok(SourceFingerprint {
        append_lsa: lh.append_lsa.to_string(),
        db_creation: lh.db_creation,
        volumes,
        db: f.db.clone(),
        databases_txt: f.databases_txt.clone(),
        log: f.log.clone(),
        vol_path: f.vol_path.clone(),
        log_path: f.log_path.clone(),
        lob_base: f.lob_base.clone(),
    })
}

/// The last line of the check report says 0 migration defects (`report::summary_line`); no report is no pass.
fn check_passed(report: &Path) -> io::Result<bool> {
    let text = match fs::read_to_string(report) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        r => r?,
    };
    Ok(text
        .lines()
        .last()
        .is_some_and(|l| l.starts_with("{\"kind\":\"summary\",\"migration\":0,")))
}

/// Removes `dir` when it is empty; a non-empty one stays.
fn remove_empty_dir(dir: &Path, deleted: &mut Vec<PathBuf>) -> io::Result<()> {
    if dir.is_dir() && fs::read_dir(dir)?.next().is_none() {
        fs::remove_dir(dir)?;
        deleted.push(dir.to_path_buf());
    }
    Ok(())
}

/// Every file below `dir` as `(size, path)`.
fn files_below(dir: &Path, out: &mut Vec<(u64, PathBuf)>) -> io::Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for e in fs::read_dir(dir)? {
        let e = e?;
        if e.file_type()?.is_dir() {
            files_below(&e.path(), out)?;
        } else {
            out.push((e.metadata()?.len(), e.path()));
        }
    }
    Ok(())
}

fn listed(v: &[(u64, PathBuf)]) -> String {
    let items: Vec<String> = v
        .iter()
        .map(|(s, p)| {
            format!(
                "{{\"path\": {}, \"size\": {s}}}",
                json_str(&p.to_string_lossy())
            )
        })
        .collect();
    format!("[{}]", items.join(", "))
}

pub fn run(argv: &[String]) -> Result<(), CliError> {
    let p = parse(argv, &[], &[])?;
    let [dir] = p.positional.as_slice() else {
        return Err(CliError::Usage(
            "finalize takes the target directory".into(),
        ));
    };
    finalize(&std::path::absolute(dir)?)
}

/// Deletes the source of the migration in `dir` (a target directory), guards first.
pub fn finalize(dir: &Path) -> Result<(), CliError> {
    let fm = dir.join("fastmig");

    // guards, all before the first deletion
    if dir.join(MARKER_NAME).exists() {
        return Err(refuse(format!(
            "{} holds {MARKER_NAME}: the migration did not finish",
            dir.display()
        )));
    }
    if !check_passed(&fm.join(CHECK_REPORT))? {
        return Err(refuse(format!(
            "{} does not say 0 migration defects: run the check first (`migrate --check`, or `cubrid-fastmig check <target> --source <source> --measured-classes {}`; ADR 0015)",
            fm.join(CHECK_REPORT).display(),
            fm.join("measured-classes").display()
        )));
    }
    let recorded = SourceFingerprint::read(&fm.join(source_fingerprint::FILE_NAME))?;
    let changes = recorded.changes(&current(&recorded)?);
    if !changes.is_empty() {
        return Err(refuse(format!(
            "the source changed since migrate: {}",
            changes.join("; ")
        )));
    }
    let manifest = match fm.join("lob-manifest") {
        m if m.exists() => lob_manifest::read(&m)?,
        _ => Vec::new(),
    };
    let base = recorded.lob_base.clone();
    if let Some(base) = &base {
        for e in &manifest {
            match fs::metadata(base.join(&e.rel_path)) {
                Ok(m) if m.len() != e.size => {
                    return Err(refuse(format!(
                        "LOB file {} is {} B, the manifest says {} B",
                        e.rel_path.display(),
                        m.len(),
                        e.size
                    )));
                }
                _ => {}
            }
        }
    }

    // delete: volumes and logs, LOB files then their emptied directories, Java directories, the registration
    let mut deleted = Vec::new();
    for (_, v) in &recorded.volumes {
        remove_file(v, &mut deleted)?;
    }
    for f in log_files(&recorded.log_path, &recorded.db)? {
        remove_file(&f, &mut deleted)?;
    }
    let mut orphans = Vec::new();
    if let Some(base) = &base {
        for e in &manifest {
            remove_file(&base.join(&e.rel_path), &mut deleted)?;
        }
        if base.is_dir() {
            for e in fs::read_dir(base)? {
                let e = e?;
                if e.file_name().to_string_lossy().starts_with("ces_") {
                    remove_empty_dir(&e.path(), &mut deleted)?;
                }
            }
        }
        files_below(base, &mut orphans)?;
        remove_empty_dir(base, &mut deleted)?;
    }
    for d in ["java", "java_static"] {
        let p = recorded.vol_path.join(d);
        if p.is_dir() {
            fs::remove_dir_all(&p)?;
            deleted.push(p);
        }
    }
    unregister(&recorded.databases_txt, &recorded.db)?;

    let outside = match fm.join(OUTSIDE_BASE) {
        m if m.exists() => lob_manifest::read(&m)?
            .into_iter()
            .map(|e| (e.size, e.rel_path))
            .collect(),
        _ => Vec::new(),
    };
    let deleted_json: Vec<String> = deleted
        .iter()
        .map(|p| json_str(&p.to_string_lossy()))
        .collect();
    let report = format!(
        "{{\n  \"source\": {},\n  \"databases_txt_line_removed\": {},\n  \"deleted\": [\n    {}\n  ],\n  \"kept_orphan_lob_files\": {},\n  \"kept_outside_base_lob_files\": {}\n}}\n",
        json_str(&recorded.db),
        json_str(&recorded.databases_txt.to_string_lossy()),
        deleted_json.join(",\n    "),
        listed(&orphans),
        listed(&outside)
    );
    fs::write(fm.join("finalize.json"), &report)?;
    // finalize.json is the tool's too: a later --replace of this target removes it with the rest (workspace#605)
    let created = fm.join(CREATED_PATHS);
    if created.exists() {
        let mut f = fs::OpenOptions::new().append(true).open(&created)?;
        std::io::Write::write_all(
            &mut f,
            format!("{}\n", fm.join("finalize.json").display()).as_bytes(),
        )?;
        f.sync_all()?;
    }
    println!(
        "finalize: deleted {} paths of {}, kept {} orphan LOB files",
        deleted.len(),
        recorded.db,
        orphans.len()
    );
    Ok(())
}
