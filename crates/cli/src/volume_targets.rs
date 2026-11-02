//! Where each source volume goes on the target: `-F` for volume 0 and, by default, the others; `-E,
//! --extended-volume-path <dir>` for volumes 1..; `-i, --control-file <file>` for any volume by its full path. The two
//! options exclude each other, as in copydb.
//!
//! Input: the options, the target directory and DB name, the source volumes as read (volid, path). Output: one target
//! path per source volume, in the source's order. A volume the control file does not list goes where it would
//! without the file.
//! Control file: one `volid source_fullvolname dest_fullvolname` per line, `#` starts a comment; the source path is
//! the one the tool reads the volume from (with `--volumes-in-first-dir`, next to volume 0).
//! Names: volume 0 is `<new-db>`, volume n `<new-db>_x<nnn>` (the engine's `fileio_make_volume_ext_name` with the
//! default prefix), unless the control file names the file.
//! Source: engine `util_sa.c:copydb` (`-E`, `-i`, exclusive), cubrid-manual admin_utils copydb `--control-file`;
//! decision G13 workspace#604 (copydb's format and meaning; clone or copy is decided per volume by its own target
//! directory, `fastmig_plan::copy_method`).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::args::{CliError, Parsed};

pub fn default_name(dir: &Path, name: &str, volid: i16) -> PathBuf {
    if volid == 0 {
        dir.join(name)
    } else {
        dir.join(format!("{name}_x{volid:03}"))
    }
}

/// `-E` or `-i` as given (checked to exclude each other).
#[derive(Clone, Debug, Default)]
pub struct VolumeLayout {
    extended: Option<PathBuf>,
    control: Option<PathBuf>,
}

impl VolumeLayout {
    pub fn from(p: &Parsed) -> Result<Self, CliError> {
        let extended = p
            .value("--extended-volume-path")
            .map(std::path::absolute)
            .transpose()?;
        let control = p.value("--control-file").map(PathBuf::from);
        if extended.is_some() && control.is_some() {
            return Err(CliError::Usage(
                "-E, --extended-volume-path and -i, --control-file exclude each other".into(),
            ));
        }
        Ok(VolumeLayout { extended, control })
    }

    /// `-E` as given, absolute.
    pub fn extended(&self) -> Option<&Path> {
        self.extended.as_deref()
    }
}

pub fn targets(
    layout: &VolumeLayout,
    target_dir: &Path,
    name: &str,
    source: &[(i16, PathBuf)],
) -> Result<Vec<PathBuf>, CliError> {
    let usage = |m: String| CliError::Usage(m);
    let extended = &layout.extended;
    let control = layout.control.as_deref().map(|c| c.display().to_string());
    let mut out: Vec<PathBuf> = source
        .iter()
        .map(|(volid, _)| match (&extended, *volid) {
            (Some(e), v) if v > 0 => default_name(e, name, v),
            (_, v) => default_name(target_dir, name, v),
        })
        .collect();
    if let Some(file) = &control {
        let text = std::fs::read_to_string(file)
            .map_err(|e| usage(format!("--control-file {file}: {e}")))?;
        let mut seen = HashSet::new();
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let f: Vec<&str> = line.split_whitespace().collect();
            let [volid, src, dst] = f.as_slice() else {
                return Err(usage(format!(
                    "--control-file {file}: bad line {line:?} (volid source_fullvolname dest_fullvolname)"
                )));
            };
            let volid: i16 = volid
                .parse()
                .map_err(|_| usage(format!("--control-file {file}: bad volid in {line:?}")))?;
            let at = source
                .iter()
                .position(|(v, _)| *v == volid)
                .ok_or_else(|| {
                    usage(format!(
                        "--control-file {file}: the source has no volume {volid}"
                    ))
                })?;
            if Path::new(src) != source[at].1 {
                return Err(usage(format!(
                    "--control-file {file}: volume {volid} is read from {}, the file says {src}",
                    source[at].1.display()
                )));
            }
            if !seen.insert(volid) {
                return Err(usage(format!(
                    "--control-file {file}: volume {volid} listed twice"
                )));
            }
            out[at] = std::path::absolute(dst)?;
        }
    }
    let mut unique = HashSet::new();
    if let Some(dup) = out.iter().find(|t| !unique.insert(*t)) {
        return Err(usage(format!("two volumes go to {}", dup.display())));
    }
    Ok(out)
}
