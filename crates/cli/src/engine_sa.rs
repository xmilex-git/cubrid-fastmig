//! Runs a CUBRID install's `csql` and `cubrid` utilities in SA mode on one registered database: the only place the
//! tool starts an engine process, and only after the migration (ADR 0018).
//!
//! Input: the install, the `databases.txt` that registers the database, a work directory. Output: per run its exit
//! code and three files in the work directory: `<name>.out` (csql's results, `-o`), `<name>.log` (stdout: csql's query
//! plans, a utility's messages) and `<name>.err` (stderr: csql's `ERROR` lines). Nothing is held in memory; callers
//! read the lines they need from the files (`csql_output`).
//! Environment of each run: `CUBRID`, `CUBRID_DATABASES` (the directory of that databases.txt), `PATH` and
//! `LD_LIBRARY_PATH` led by the install; stdin is empty, so a password prompt reads EOF instead of waiting; the cwd is
//! the work directory, where csql and the utilities leave their own logs.
//! Source: ADR 0018 (workspace#611 decision 1); files per run: user instruction relayed by the 1번 session
//! 2026-11-02; e2e/run.sh (the target install's environment).

use std::ffi::OsString;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub struct EngineSa {
    install: PathBuf,
    databases_dir: PathBuf,
    work_dir: PathBuf,
}

pub struct RunOutput {
    /// `None`: ended by a signal.
    pub code: Option<i32>,
    /// csql's `-o` file; `None` for a utility.
    pub out: Option<PathBuf>,
    pub log: PathBuf,
    pub err: PathBuf,
}

impl EngineSa {
    /// The engine reads `$CUBRID_DATABASES/databases.txt`: a file of another name cannot register the database.
    pub fn new(install: &Path, databases_txt: &Path, work_dir: &Path) -> io::Result<Self> {
        if databases_txt
            .file_name()
            .is_none_or(|n| n != "databases.txt")
        {
            return Err(io::Error::other(format!(
                "{} is not named databases.txt: the engine cannot find the database through it",
                databases_txt.display()
            )));
        }
        for bin in ["csql", "cubrid"] {
            let p = install.join("bin").join(bin);
            if !p.is_file() {
                return Err(io::Error::other(format!(
                    "{} is missing: not a CUBRID install",
                    p.display()
                )));
            }
        }
        fs::create_dir_all(work_dir)?;
        Ok(EngineSa {
            install: install.to_path_buf(),
            databases_dir: databases_txt
                .parent()
                .unwrap_or(Path::new("."))
                .to_path_buf(),
            work_dir: work_dir.to_path_buf(),
        })
    }

    fn with_install(&self, var: &str, sub: &str) -> OsString {
        let mut v = self.install.join(sub).into_os_string();
        if let Some(old) = std::env::var_os(var).filter(|o| !o.is_empty()) {
            v.push(":");
            v.push(old);
        }
        v
    }

    /// `<install>/bin/<program> <args>`: stdout to `<name>.log`, stderr to `<name>.err`.
    pub fn run(&self, name: &str, program: &str, args: &[&str]) -> io::Result<RunOutput> {
        let log = self.work_dir.join(format!("{name}.log"));
        let err = self.work_dir.join(format!("{name}.err"));
        let status = Command::new(self.install.join("bin").join(program))
            .args(args)
            .current_dir(&self.work_dir)
            .env("CUBRID", &self.install)
            .env("CUBRID_DATABASES", &self.databases_dir)
            .env("PATH", self.with_install("PATH", "bin"))
            .env(
                "LD_LIBRARY_PATH",
                self.with_install("LD_LIBRARY_PATH", "lib"),
            )
            .stdin(Stdio::null())
            .stdout(File::create(&log)?)
            .stderr(File::create(&err)?)
            .status()?;
        Ok(RunOutput {
            code: status.code(),
            out: None,
            log,
            err,
        })
    }

    /// `csql -S -u dba -i <file> -o <work>/<name>.out <db>`; `query_output`: `-q -N`, rows as SQL literals without
    /// column names.
    pub fn csql_file(
        &self,
        name: &str,
        db: &str,
        file: &Path,
        query_output: bool,
    ) -> io::Result<RunOutput> {
        let out = self.work_dir.join(format!("{name}.out"));
        let (file, out_arg) = (file.to_string_lossy(), out.to_string_lossy());
        let mut args = vec!["-S", "-u", "dba", "-p", "", "-i", &file, "-o", &out_arg];
        if query_output {
            args.extend(["-q", "-N"]);
        }
        args.push(db);
        let mut r = self.run(name, "csql", &args)?;
        r.out = Some(out);
        Ok(r)
    }

    /// Writes `sql` to `<work>/<name>.sql` and runs it with `-q -N`.
    pub fn csql_query(&self, name: &str, db: &str, sql: &str) -> io::Result<RunOutput> {
        let file = self.work_dir.join(format!("{name}.sql"));
        fs::write(&file, sql)?;
        self.csql_file(name, db, &file, true)
    }
}
