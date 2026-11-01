//! The target release line read from the target install `$CUBRID`: the default of `--target-version`.
//!
//! Input: the `CUBRID` environment variable. Output: the line (`11.4` or `develop`), or a usage error that names
//! `--target-version` when the install is missing or of another line.
//! How: `$CUBRID/lib/libcubrid.so` is a symlink to `libcubrid.so.<major>.<minor>`, the engine's SOVERSION; no engine
//! process is started. Measured on the dev host: develop 11.5.0.2781 → `.11.5`, 11.4.6 → `.11.4`, 11.3.6 → `.11.3`,
//! 10.2.18 → `.10.2` (workspace#605).
//! Source: engine `cubrid/CMakeLists.txt` `SOVERSION "${CUBRID_MAJOR_VERSION}.${CUBRID_MINOR_VERSION}"` (develop
//! 5b1867174); decision G13 workspace#604 (the install's version, an error when it cannot be read; the command line
//! wins).

use std::path::PathBuf;

use fastmig_ondisk::VersionLine;

use crate::args::CliError;

fn usage(why: String) -> CliError {
    CliError::Usage(format!("{why}; pass --target-version 11.4|develop"))
}

pub fn from_install() -> Result<VersionLine, CliError> {
    let cubrid = std::env::var_os("CUBRID")
        .map(PathBuf::from)
        .ok_or_else(|| {
            usage("cannot read the target install's version: CUBRID is not set".into())
        })?;
    let lib = cubrid.join("lib/libcubrid.so");
    let link = std::fs::read_link(&lib).map_err(|e| {
        usage(format!(
            "cannot read the target install's version from {}: {e}",
            lib.display()
        ))
    })?;
    let name = link
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let version = name.strip_prefix("libcubrid.so.").unwrap_or("");
    let (major, minor) = version
        .split_once('.')
        .and_then(|(a, b)| Some((a.parse::<u32>().ok()?, b.parse::<u32>().ok()?)))
        .ok_or_else(|| {
            usage(format!(
                "{} points to {name}, not libcubrid.so.<major>.<minor>",
                lib.display()
            ))
        })?;
    match (major, minor) {
        (11, 4) => Ok(VersionLine::V11_4),
        // develop carries the next minor (11.5 at the time of writing)
        (m, n) if (m, n) > (11, 4) => Ok(VersionLine::Develop),
        _ => Err(usage(format!(
            "the target install {} is CUBRID {major}.{minor}: not a target line (11.4 or develop)",
            cubrid.display()
        ))),
    }
}
