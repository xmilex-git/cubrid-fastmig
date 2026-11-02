//! The target directories the options name: checked by `plan` so `migrate` refuses before its first write instead of
//! stopping in a copy phase with `FASTMIG_INCOMPLETE` left behind, and the missing ones `migrate` may create, created.
//! - `-E, --extended-volume-path` (volumes 1..) and `-L, --log-path`: must exist, be a directory and be writable.
//! - `-B, --lob-base-path` when given: as createdb and copydb, a missing last level is created (mode 0700) when its
//!   parent exists; a missing parent is refused. The default `<-F>/lob` is created as before.
//! - the directories of `-i, --control-file` targets: a missing one is created with its parents; its nearest existing
//!   ancestor must be a writable directory.
//!
//! A directory equal to `-F` is skipped (migrate creates `-F`). What migrate creates is recorded in the incomplete
//! marker first, so it lands in `created-paths` and a rerun or `--replace` removes it.
//! Input: the [`PlanOptions`] and the volume targets. Output: one [`TargetDirProblem`] per directory that fails, for
//! the plan's refusals and the path rows; [`create`] for migrate.
//! Source: workspace#614 decisions 1, 4, 5; engine `boot_sr.c:xboot_create` (a missing `-F` or `-L` is
//! `ER_BO_DIRECTORY_DOESNOT_EXIST`; the lob path is `mkdir (p, 0700)`), `xboot_copy` (the same `mkdir` of the lob
//! path); POSIX `access(2)`.

use std::ffi::CString;
use std::fs::DirBuilder;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

use fastmig_copy::IncompleteMarker;

use crate::plan::PlanOptions;

/// `W_OK | X_OK`: create files in the directory.
const W_OK_X_OK: i32 = 2 | 1;

unsafe extern "C" {
    fn access(path: *const std::ffi::c_char, mode: i32) -> i32;
}

pub enum Problem {
    Missing,
    /// `-B`: its parent is missing too.
    MissingParent,
    NotDirectory,
    NotWritable,
}

impl Problem {
    /// Shown after the path in the path rows.
    pub fn short(&self) -> &'static str {
        match self {
            Problem::Missing => "no such directory",
            Problem::MissingParent => "no such parent directory",
            Problem::NotDirectory => "not a directory",
            Problem::NotWritable => "not writable",
        }
    }
}

pub struct TargetDirProblem {
    /// `-E, --extended-volume-path`.
    pub option: &'static str,
    pub dir: PathBuf,
    pub problem: Problem,
}

impl TargetDirProblem {
    /// The plan's refusal.
    pub fn refusal(&self) -> String {
        let fix = match self.problem {
            Problem::Missing => "create it first; migrate does not create it",
            Problem::MissingParent => {
                "create its parent first; migrate creates only the last level, as createdb"
            }
            Problem::NotDirectory => "pass a directory",
            Problem::NotWritable => "make it writable",
        };
        format!(
            "{} {}: {} ({fix})",
            self.option,
            self.dir.display(),
            self.problem.short()
        )
    }
}

fn writable(dir: &Path) -> bool {
    let Ok(c) = CString::new(dir.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `c` is a NUL-terminated string that outlives the call; access only reads it.
    unsafe { access(c.as_ptr(), W_OK_X_OK) == 0 }
}

/// None when `dir` exists as a writable directory; `missing` when it does not exist.
fn problem(dir: &Path, missing: Problem) -> Option<Problem> {
    match dir.metadata() {
        Err(_) => Some(missing),
        Ok(m) if !m.is_dir() => Some(Problem::NotDirectory),
        Ok(_) if !writable(dir) => Some(Problem::NotWritable),
        Ok(_) => None,
    }
}

/// `-B` as given; none when it is the default `<-F>/lob`.
fn given_lob_dir(o: &PlanOptions) -> Option<&Path> {
    Some(o.lob_dir.as_path()).filter(|d| *d != o.target_dir.join("lob"))
}

/// The directories of the `-i` targets other than `-F`, each once.
fn control_dirs<'a>(o: &PlanOptions, targets: &'a [PathBuf]) -> Vec<&'a Path> {
    let mut dirs: Vec<&Path> = Vec::new();
    if o.layout.control().is_some() {
        for d in targets.iter().filter_map(|t| t.parent()) {
            if d != o.target_dir && !dirs.contains(&d) {
                dirs.push(d);
            }
        }
    }
    dirs
}

pub fn check(o: &PlanOptions, targets: &[PathBuf]) -> Vec<TargetDirProblem> {
    let mut out = Vec::new();
    let mut push = |option, dir: &Path, p: Option<Problem>| {
        if let Some(problem) = p {
            out.push(TargetDirProblem {
                option,
                dir: dir.to_path_buf(),
                problem,
            });
        }
    };
    let given = [
        ("-E, --extended-volume-path", o.layout.extended()),
        ("-L, --log-path", Some(o.log_dir.as_path())),
    ];
    for (option, dir) in given {
        if let Some(dir) = dir.filter(|d| *d != o.target_dir) {
            push(option, dir, problem(dir, Problem::Missing));
        }
    }
    // -B: the last level is created when the parent is there (workspace#614 decision 4)
    if let Some(dir) = given_lob_dir(o) {
        let option = "-B, --lob-base-path";
        match (problem(dir, Problem::Missing), dir.parent()) {
            (Some(Problem::Missing), Some(parent)) => match problem(parent, Problem::MissingParent)
            {
                Some(Problem::MissingParent) => push(option, dir, Some(Problem::MissingParent)),
                // the parent is a file or not writable: that is what to fix
                p => push(option, parent, p),
            },
            (p, _) => push(option, dir, p),
        }
    }
    // -i: a missing directory is created with its parents (workspace#614 decision 5)
    for dir in control_dirs(o, targets) {
        if let Some(ancestor) = dir.ancestors().find(|a| a.exists()) {
            push(
                "-i, --control-file",
                ancestor,
                problem(ancestor, Problem::Missing),
            );
        }
    }
    out
}

/// Creates what [`check`] let through as missing, recorded in `marker` first: the `-B` last level (mode 0700, as
/// createdb) and the `-i` target directories with their parents.
pub fn create(o: &PlanOptions, targets: &[PathBuf], marker: &IncompleteMarker) -> io::Result<()> {
    if let Some(dir) = given_lob_dir(o).filter(|d| !d.exists()) {
        marker.record(dir)?;
        DirBuilder::new().mode(0o700).create(dir)?;
    }
    for dir in control_dirs(o, targets) {
        marker.create_dir_all(dir)?;
    }
    Ok(())
}
