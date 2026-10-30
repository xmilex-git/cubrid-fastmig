//! `cubrid-fastmig template-extract`: a build-time step, not a migration command. Writes the catalog template of a
//! database the target engine just created; `crates/catalog/template/build.sh` runs it.
//!
//! Input: `<first volume of the fresh DB> --out <file> --install <dir> --release <engine release> --locale <name>
//! [--line 11.4|develop]` (the target line, default develop).
//! Output: the template text file.
//! Source: ADR 0011, workspace#482 decision 2.

use std::path::Path;

use fastmig_catalog::template_extract::extract;
use fastmig_ondisk::source_version::HeaderLayout;
use fastmig_ondisk::volume_set::{Access, Locate, VolumeSet};
use fastmig_plan::version_pair::{line_name, parse_line};

use crate::args::{self, CliError};

pub fn run(argv: &[String]) -> Result<(), CliError> {
    let p = args::parse(
        argv,
        &["--out", "--install", "--release", "--locale", "--line"],
        &[],
    )?;
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
    let vs = VolumeSet::open(
        Path::new(first),
        Locate::FirstVolumeDir,
        Access::ReadOnly,
        HeaderLayout::Since11_4,
    )?;
    // a template per target line: develop (workspace#482), 11.4 (workspace#533)
    let line_arg = p.value("--line").unwrap_or("develop");
    let line = parse_line(line_arg)
        .ok_or_else(|| CliError::Usage(format!("--line {line_arg}: 11.4 or develop")))?;
    let meta = vec![
        ("line".into(), line_name(line).into()),
        ("install".into(), need("--install")?.into()),
        ("engine_release".into(), need("--release")?.into()),
        ("locale".into(), need("--locale")?.into()),
        ("io_pagesize".into(), vs.io_pagesize().to_string()),
    ];
    let t = extract(&vs, line, meta)?;
    std::fs::write(out, t.to_text())?;
    println!("{out}: {} classes, {} rows", t.classes.len(), t.rows.len());
    Ok(())
}
