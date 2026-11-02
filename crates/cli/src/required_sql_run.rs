//! `--run-sql`: runs the required SQL files on the registered target with the target install's csql in SA mode, in
//! the order `required_sql` sets, and stops at the first file that fails.
//!
//! Input: the target's SA runner, its name, `<dest>/fastmig/` and the files of the required SQL list. Output: per file
//! that ran, its seconds and the first failed statement (`ERROR` in the output: csql exits 0 after a failed
//! statement, `csql_output`); the files after a failed one do not run. `post-migration-fixes.sql` is never run: the
//! DBA reviews it first (workspace#611 decision 4).
//! Source: workspace#611 decision 2; e2e/run.sh `sql_file` (the same rule, workspace#555).

use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::csql_output::{SqlError, first_error, last_line};
use crate::engine_sa::EngineSa;

pub struct SqlFileRun {
    pub file: String,
    pub seconds: f64,
    pub error: Option<SqlError>,
    /// csql's stderr file of a file that ran (`<dest>/fastmig/post/sql-<name>.err`).
    pub err_file: Option<PathBuf>,
    /// Not run again: an earlier post run ran it without error (its statements, e.g. CREATE INDEX, would now fail).
    pub earlier: bool,
}

/// `ran_before`: files an earlier run of post ran without error; they are skipped.
pub fn run(
    engine: &EngineSa,
    db: &str,
    fastmig_dir: &Path,
    files: &[String],
    ran_before: &[String],
) -> io::Result<Vec<SqlFileRun>> {
    let mut runs = Vec::new();
    for f in files {
        if ran_before.contains(f) {
            runs.push(SqlFileRun {
                file: f.clone(),
                seconds: 0.0,
                error: None,
                err_file: None,
                earlier: true,
            });
            continue;
        }
        let t = Instant::now();
        let name = format!("sql-{}", f.trim_end_matches(".sql"));
        let out = engine.csql_file(&name, db, &fastmig_dir.join(f), false)?;
        // csql writes every ERROR to stderr, none to its -o file (workspace#611 scratch/611 probe-o)
        let error = match first_error(&out.err)? {
            Some(e) => Some(e),
            None if out.code != Some(0) => Some(SqlError {
                line: None,
                text: format!("csql exited {:?}: {}", out.code, last_line(&out.err)?),
            }),
            None => None,
        };
        let failed = error.is_some();
        runs.push(SqlFileRun {
            file: f.clone(),
            seconds: t.elapsed().as_secs_f64(),
            error,
            err_file: Some(out.err),
            earlier: false,
        });
        if failed {
            break;
        }
    }
    Ok(runs)
}
