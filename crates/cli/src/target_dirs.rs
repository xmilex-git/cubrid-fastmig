//! The given target directories migrate writes into but does not create: `-E, --extended-volume-path` (volumes 1..)
//! and `-L, --log-path` (the active log, `_vinf`). Each must exist, be a directory and be writable, checked by `plan`
//! so `migrate` refuses before its first write instead of stopping in a copy phase with `FASTMIG_INCOMPLETE` left
//! behind; a directory equal to `-F` is skipped (migrate creates `-F`).
//!
//! Input: the [`PlanOptions`]. Output: one [`TargetDirProblem`] per directory that fails, for the plan's refusals and
//! the path rows.
//! Source: workspace#614 decision 1 (refuse, never create); engine `boot_sr.c:xboot_create` (a missing `-F` or `-L`
//! is `ER_BO_DIRECTORY_DOESNOT_EXIST`), `copydb` (a missing `-E` or `-L` fails when the volume is mounted); POSIX
//! `access(2)`.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use crate::plan::PlanOptions;

/// `W_OK | X_OK`: create files in the directory.
const W_OK_X_OK: i32 = 2 | 1;

unsafe extern "C" {
    fn access(path: *const std::ffi::c_char, mode: i32) -> i32;
}

pub enum Problem {
    Missing,
    NotDirectory,
    NotWritable,
}

impl Problem {
    /// Shown after the path in the path rows.
    pub fn short(&self) -> &'static str {
        match self {
            Problem::Missing => "no such directory",
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
            Problem::NotWritable => "make it writable",
            _ => "create it first; migrate does not create it",
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

pub fn check(o: &PlanOptions) -> Vec<TargetDirProblem> {
    let given = [
        ("-E, --extended-volume-path", o.layout.extended()),
        ("-L, --log-path", Some(o.log_dir.as_path())),
    ];
    given
        .into_iter()
        .filter_map(|(option, dir)| {
            let dir = dir.filter(|d| *d != o.target_dir)?;
            let problem = match dir.metadata() {
                Err(_) => Problem::Missing,
                Ok(m) if !m.is_dir() => Problem::NotDirectory,
                Ok(_) if !writable(dir) => Problem::NotWritable,
                Ok(_) => return None,
            };
            Some(TargetDirProblem {
                option,
                dir: dir.to_path_buf(),
                problem,
            })
        })
        .collect()
}
