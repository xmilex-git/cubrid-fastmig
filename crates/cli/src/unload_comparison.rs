//! `--source-cubrid <install>`: the whole data comparison of the engine check: the source engine unloads a copy of the
//! source, the target engine unloads the target, and `set-comparison` compares the two with the version pair's rules,
//! as e2e/run.sh does.
//!
//! Input: the source fingerprint, the source install, the target's SA runner and name, the version pair, the work
//! directory `<dest>/fastmig/post/`. Output: [`Comparison`]: same or not, with the set-comparison output and diff
//! files; a refusal when the source copy does not fit.
//! Left on disk: `set-comparison.out`, `set-comparison.diff` and the unload logs. The source copy is removed whatever
//! the outcome; the unload files are removed when the sets are the same (kept for reading otherwise). Both
//! directories are in `created-paths` before they are made, so `--replace` removes what a killed run left, and
//! the next post run removes them first.
//! Source: workspace#611 decision 3 (e); e2e/run.sh steps 6 (target unloaddb, set-comparison with the pair).

use std::fs;
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::engine_sa::EngineSa;
use crate::source_fingerprint::SourceFingerprint;
use crate::source_unload_copy::{self, NoSpace};

pub const SOURCE_COPY: &str = "source-copy";
pub const SOURCE_UNLOAD: &str = "unload-source";
pub const TARGET_UNLOAD: &str = "unload-target";

pub enum Comparison {
    /// The source copy would not fit: nothing was written.
    Refused(String),
    /// A step failed before the comparison: which one and its output file.
    Failed(String),
    Compared {
        same: bool,
        output: String,
    },
}

/// Removes what an earlier run left in `work`.
pub fn clear(work: &Path) -> io::Result<()> {
    for d in [SOURCE_COPY, SOURCE_UNLOAD, TARGET_UNLOAD] {
        let p = work.join(d);
        if p.exists() {
            fs::remove_dir_all(&p)?;
        }
    }
    Ok(())
}

fn unload(engine: &EngineSa, name: &str, db: &str, dir: &Path) -> io::Result<Result<(), String>> {
    fs::create_dir(dir)?;
    let out = engine.run(
        name,
        "cubrid",
        &["unloaddb", "-S", "-O", &dir.to_string_lossy(), db],
    )?;
    Ok(match out.code {
        Some(0) => Ok(()),
        c => Err(format!(
            "{name} exited {c:?} ({}, {})",
            out.err.display(),
            out.log.display()
        )),
    })
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    fp: &SourceFingerprint,
    source_install: &Path,
    target: &EngineSa,
    target_db: &str,
    pair: (&str, &str),
    work: &Path,
    record: &dyn Fn(&Path) -> io::Result<()>,
) -> io::Result<Comparison> {
    let copy = work.join(SOURCE_COPY);
    let (src_dir, tgt_dir) = (work.join(SOURCE_UNLOAD), work.join(TARGET_UNLOAD));
    for d in [&copy, &src_dir, &tgt_dir] {
        record(d)?;
    }
    let dbtxt = match source_unload_copy::make(fp, &copy)? {
        Ok(t) => t,
        Err(NoSpace(need, free, dir)) => {
            return Ok(Comparison::Refused(format!(
                "the source copy for the data comparison needs {need} B of byte copies in {}, {free} B are free",
                dir.display()
            )));
        }
    };
    let source = EngineSa::new(source_install, &dbtxt, work)?;
    let unloaded = unload(&source, "source-unloaddb", &fp.db, &src_dir);
    fs::remove_dir_all(&copy)?;
    if let Err(e) = unloaded? {
        return Ok(Comparison::Failed(e));
    }
    if let Err(e) = unload(target, "target-unloaddb", target_db, &tgt_dir)? {
        return Ok(Comparison::Failed(e));
    }
    let output = work.join("set-comparison.out");
    let status = Command::new(std::env::current_exe()?)
        .arg("set-comparison")
        .arg(src_dir.join(&fp.db))
        .arg(tgt_dir.join(target_db))
        .arg("--diff")
        .arg(work.join("set-comparison.diff"))
        .args(["--source-version", pair.0, "--target-version", pair.1])
        .stdin(Stdio::null())
        .stdout(fs::File::create(&output)?)
        .stderr(Stdio::inherit())
        .status()?;
    match status.code() {
        Some(0) => {
            fs::remove_dir_all(&src_dir)?;
            fs::remove_dir_all(&tgt_dir)?;
            Ok(Comparison::Compared {
                same: true,
                output: output.display().to_string(),
            })
        }
        Some(1) => Ok(Comparison::Compared {
            same: false,
            output: output.display().to_string(),
        }),
        c => Ok(Comparison::Failed(format!(
            "set-comparison exited {c:?} ({})",
            output.display()
        ))),
    }
}
