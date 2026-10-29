//! `cubrid-fastmig finalize`: deletes the source database's files once its migration verified and the source is
//! unchanged — the only command that deletes anything of the source.
//!
//! Input: the target directory of a finished migrate (`<dest>/fastmig/` holds the source fingerprint, the LOB
//! manifests and the verify report). Output: the source's files removed, its databases.txt line removed, and
//! `<dest>/fastmig/finalize.json` listing what was deleted and the LOB files kept.
//! Guards: no incomplete marker in the target; the verify report's summary has 0 migration defects; the source
//! fingerprint (log header append_lsa and db_creation read under the read lock, every volume's size, every
//! referenced LOB file's size) equals the one migrate recorded.
//! Deleted: the recorded volumes; `<db>_lgat`, `<db>_lgar*`, `<db>_lginf`, `<db>_vinf`, `<db>_dwb` of the log
//! directory; the referenced LOB files, then `ces_*` directories and the lob-base directory left empty; `java` and
//! `java_static` of the volume directory. Kept: backups, `_bkvinf`, `_keys`, orphan LOB files and files outside the
//! lob-base-path (listed in finalize.json).
//! Source: decision G1 workspace#481 (guard and delete list), G6 workspace#488 8-10 (LOB and Java files), ADR 0008.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use fastmig_copy::incomplete_marker::MARKER_NAME;
use fastmig_copy::lob_manifest;
use fastmig_copy::open_locked_log;
use fastmig_verify::report::json_str;

use crate::args::{CliError, parse};
use crate::plan::read_log_header;
use crate::source_fingerprint::{self, SourceFingerprint};

const HELP: &str = "\
cubrid-fastmig finalize <target-dir>
  Deletes the source of the migration in <target-dir> once it verified and the source is unchanged (ADR 0008).";

/// `<dest>/fastmig/lob-outside-base`: LOB files the source names by an absolute path outside its lob-base-path
/// (`<size>\t<absolute path>` lines), kept by finalize.
pub const OUTSIDE_BASE: &str = "lob-outside-base";

fn refuse(msg: String) -> CliError {
    CliError::Failed(io::Error::other(format!("finalize refused: {msg}")))
}

/// The source as it is now, read the way migrate recorded it.
fn current(f: &SourceFingerprint) -> io::Result<SourceFingerprint> {
    // the read lock refuses a source whose server is up
    let lgat = open_locked_log(&f.log)?;
    let lh = read_log_header(&lgat, &f.log)?;
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

/// The last line of the verify report says 0 migration defects (`report::summary_line`).
fn verify_passed(report: &Path) -> io::Result<bool> {
    let text = fs::read_to_string(report)?;
    Ok(text
        .lines()
        .last()
        .is_some_and(|l| l.starts_with("{\"kind\":\"summary\",\"migration\":0,")))
}

/// Log-directory files of the database (G1): the exact names, plus every `<db>_lgar<digits>` archive and `<db>_lgar_t`.
fn log_files(dir: &Path, db: &str) -> io::Result<Vec<PathBuf>> {
    let exact: Vec<String> = ["_lgat", "_lginf", "_vinf", "_dwb"]
        .iter()
        .map(|s| format!("{db}{s}"))
        .collect();
    let archive = format!("{db}_lgar");
    let mut out = Vec::new();
    for e in fs::read_dir(dir)? {
        let name = e?.file_name().to_string_lossy().into_owned();
        if exact.contains(&name)
            || name.strip_prefix(&archive).is_some_and(|r| {
                r == "_t" || (!r.is_empty() && r.bytes().all(|b| b.is_ascii_digit()))
            })
        {
            out.push(dir.join(name));
        }
    }
    out.sort();
    Ok(out)
}

fn remove_file(p: &Path, deleted: &mut Vec<PathBuf>) -> io::Result<()> {
    match fs::remove_file(p) {
        Ok(()) => {
            deleted.push(p.to_path_buf());
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io::Error::new(e.kind(), format!("{}: {e}", p.display()))),
    }
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

/// Removes the line of `db` from databases.txt (rename over it, so a crash leaves the old or the new file).
fn unregister(dbtxt: &Path, db: &str) -> io::Result<()> {
    let text = fs::read_to_string(dbtxt)?;
    let kept: String = text
        .lines()
        .filter(|l| l.split('#').next().unwrap_or("").split_whitespace().next() != Some(db))
        .map(|l| format!("{l}\n"))
        .collect();
    let tmp = dbtxt.with_extension("fastmig-finalize");
    fs::write(&tmp, kept)?;
    fs::File::open(&tmp)?.sync_all()?;
    fs::rename(&tmp, dbtxt)
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
    let p = parse(argv, &[], &["--help"])?;
    if p.switch("--help") {
        println!("{HELP}");
        return Ok(());
    }
    let [dir] = p.positional.as_slice() else {
        return Err(CliError::Usage(
            "finalize takes the target directory".into(),
        ));
    };
    let dir = std::path::absolute(dir)?;
    let fm = dir.join("fastmig");

    // guards, all before the first deletion
    if dir.join(MARKER_NAME).exists() {
        return Err(refuse(format!(
            "{} holds {MARKER_NAME}: the migration did not verify",
            dir.display()
        )));
    }
    if !verify_passed(&fm.join("verify.jsonl"))? {
        return Err(refuse(
            "the verify report does not say 0 migration defects".into(),
        ));
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
    println!(
        "finalize: deleted {} paths of {}, kept {} orphan LOB files",
        deleted.len(),
        recorded.db,
        orphans.len()
    );
    Ok(())
}
