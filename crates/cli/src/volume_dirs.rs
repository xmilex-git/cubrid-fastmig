//! Checks where the source's volumes 1.. are read from against volume 0's directory, for `plan` and `migrate`: a
//! database directory copied without its recorded paths rewritten still names the original volumes, so the tool
//! would read volume 0 and the log from the copy and the other volumes from the original (a mixed source).
//!
//! Input: the open source and how its volumes were located. Output: warnings (volumes in another directory, `_vinf`
//! lines naming other files than those read) and refusals (a volume read from another directory while a file of the
//! same name lies next to volume 0: the trace of a copy).
//! Source: engine `boot_sr.c:boot_find_rest_permanent_volumes` (`_vinf` first, header chain if it is missing or bad),
//! `boot_mount` (a path differing from the header only warns while `_vinf` exists); the registered base DB that an
//! engine run on such a copy wrote into and then deleted (workspace#515).

use std::fs;
use std::path::{Path, PathBuf};

use fastmig_ondisk::volume_set::Locate;

use crate::plan::OpenSource;

/// Warnings and refusals about the directories the source's volumes come from.
pub fn check(s: &OpenSource, locate: Locate) -> (Vec<String>, Vec<String>) {
    let (mut warnings, mut refusals) = (Vec::new(), Vec::new());
    let read: Vec<PathBuf> = s.vols.volumes().iter().map(|v| v.path.clone()).collect();
    let Some(dir0) = read.first().and_then(|p| p.parent()) else {
        return (warnings, refusals);
    };
    if locate == Locate::Recorded {
        let elsewhere: Vec<&PathBuf> = read[1..]
            .iter()
            .filter(|p| p.parent() != Some(dir0))
            .collect();
        for p in &elsewhere {
            if let Some(name) = p.file_name()
                && dir0.join(name).exists()
            {
                refusals.push(format!(
                    "volume {} lies outside volume 0's directory and {} also exists: a copied database whose volume \
                     headers name the original; pass --volumes-in-first-dir or fix the copy's _vinf and headers",
                    p.display(),
                    dir0.join(name).display()
                ));
            }
        }
        if !elsewhere.is_empty() {
            warnings.push(format!(
                "volumes outside volume 0's directory {}: {}",
                dir0.display(),
                elsewhere
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    let vinf = s.entry.log_path.join(format!("{}_vinf", s.entry.name));
    let differ: Vec<String> = vinf_volumes(&vinf)
        .into_iter()
        .filter(|(volid, p)| read.get(*volid as usize) != Some(p))
        .map(|(volid, p)| format!("{volid} {}", p.display()))
        .collect();
    if !differ.is_empty() {
        warnings.push(format!(
            "{} names other volume files than the ones read (an engine started on this source would open them): {}",
            vinf.display(),
            differ.join(", ")
        ));
    }
    (warnings, refusals)
}

/// The volume lines (volid ≥ 0) of a `_vinf` file, `%4d %s` per line; none when the file is missing or unreadable
/// (the engine then follows the headers, as the tool does).
fn vinf_volumes(path: &Path) -> Vec<(i32, PathBuf)> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| {
            let (id, p) = l.trim_start().split_once(' ')?;
            Some((id.parse::<i32>().ok()?, PathBuf::from(p.trim())))
        })
        .filter(|(id, _)| *id >= 0)
        .collect()
}
