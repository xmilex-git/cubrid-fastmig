//! `cubrid-fastmig template-extract`: a build-time step, not a migration command. Writes the catalog template of a
//! database the target engine just created; `crates/catalog/template/build.sh` runs it.
//!
//! Input: `<first volume of the fresh DB> --out <file> --install <dir> --release <engine release> --locale <name>`.
//! Output: the template text file.
//! Source: ADR 0011, workspace#482 decision 2.

use std::path::Path;

use fastmig_catalog::template_extract::extract;
use fastmig_ondisk::VersionLine;
use fastmig_ondisk::volume_set::{Access, Locate, VolumeSet};

use crate::args::{self, CliError};

pub fn run(argv: &[String]) -> Result<(), CliError> {
    let p = args::parse(argv, &["--out", "--install", "--release", "--locale"], &[])?;
    let [first] = p.positional.as_slice() else {
        return Err(CliError::Usage(
            "template-extract needs the first volume of the fresh DB".into(),
        ));
    };
    let need = |f: &str| {
        p.value(f)
            .ok_or_else(|| CliError::Usage(format!("template-extract needs {f}")))
    };
    let out = need("--out")?;
    let vs = VolumeSet::open(Path::new(first), Locate::FirstVolumeDir, Access::ReadOnly)?;
    // templates exist for the develop target only (workspace#482)
    let meta = vec![
        ("line".into(), "develop".into()),
        ("install".into(), need("--install")?.into()),
        ("engine_release".into(), need("--release")?.into()),
        ("locale".into(), need("--locale")?.into()),
        ("io_pagesize".into(), vs.io_pagesize().to_string()),
    ];
    let t = extract(&vs, VersionLine::Develop, meta)?;
    std::fs::write(out, t.to_text())?;
    println!("{out}: {} classes, {} rows", t.classes.len(), t.rows.len());
    Ok(())
}
