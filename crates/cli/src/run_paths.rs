//! The source and target path of each kind of file plan and migrate touch: volume directories, the active log and
//! the archive directory, the LOB base, the Java SP directories and databases.txt, with how the target gets each.
//!
//! Input: the plan (volume paths and their copy methods), the open source (its databases.txt entry and active log),
//! the [`PlanOptions`] (`-F`, `-L`, `-B`, `-E`/`-i` through the plan, databases.txt files, `--no-clone`, `-r`).
//! Output: the [`PathRow`]s the question screen, the plan screen, the progress screen and the end box show.
//! Volume 0's directory is one row; volumes 1.. in another directory pair get a row per pair (`-E`, `-i`). The target
//! log is new (the run writes it), the source archives are not carried over; LOB and Java directories are cloned
//! where the filesystem allows it (`copy_method::predict`, as the volumes).
//! Source: workspace#612 decision 6; G6 workspace#488 (LOB and Java copied, clone first); workspace#604 (`-L`, `-B`,
//! `-E`, `-i`).

use std::io;
use std::path::{Path, PathBuf};

use fastmig_plan::Plan;
use fastmig_plan::copy_method;
use fastmig_plan::migration_plan::lob_base_dir;
use fastmig_progress::PathRow;

use crate::plan::{OpenSource, PlanOptions};

/// Rows of volume directory pairs before the rest is counted.
const VOLUME_GROUPS: usize = 8;

fn show(p: &Path) -> String {
    std::path::absolute(p)
        .unwrap_or_else(|_| p.to_path_buf())
        .display()
        .to_string()
}

fn dir_of(p: &Path) -> &Path {
    p.parent().unwrap_or(p)
}

/// `0`, `1-3`, `1-3,7`.
fn volids(ids: &[i16]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < ids.len() {
        let mut j = i;
        while j + 1 < ids.len() && ids[j + 1] == ids[j] + 1 {
            j += 1;
        }
        out.push(if i == j {
            ids[i].to_string()
        } else {
            format!("{}-{}", ids[i], ids[j])
        });
        i = j + 1;
    }
    out.join(",")
}

/// A row of a directory the run copies as a tree: `none` when the source has no such directory.
fn tree_row(
    label: &str,
    source: Option<PathBuf>,
    target: &Path,
    allow_clone: bool,
) -> io::Result<PathRow> {
    let method = match &source {
        Some(s) if s.exists() => copy_method::predict(s, target, allow_clone)?.name(),
        _ => "none",
    };
    Ok(PathRow {
        label: label.into(),
        method,
        source: source.as_deref().map(show),
        target: show(target),
    })
}

pub fn rows(plan: &Plan, src: &OpenSource, o: &PlanOptions) -> io::Result<Vec<PathRow>> {
    let mut out = Vec::new();
    // volume directory pairs in volid order, volume 0's first
    let mut groups: Vec<(&Path, &Path, &'static str, Vec<i16>)> = Vec::new();
    for v in &plan.volumes {
        let key = (dir_of(&v.path), dir_of(&v.target), v.copy_method.name());
        match groups.iter_mut().find(|g| (g.0, g.1, g.2) == key) {
            Some(g) => g.3.push(v.volid),
            None => groups.push((key.0, key.1, key.2, vec![v.volid])),
        }
    }
    for (s, t, m, ids) in groups.iter().take(VOLUME_GROUPS) {
        out.push(PathRow {
            label: format!(
                "{} {}",
                if ids.len() == 1 { "volume" } else { "volumes" },
                volids(ids)
            ),
            method: m,
            source: Some(show(s)),
            target: show(t),
        });
    }
    if groups.len() > VOLUME_GROUPS {
        out.push(PathRow {
            label: format!("+{} more", groups.len() - VOLUME_GROUPS),
            method: "",
            source: Some("(plan --json lists every volume)".into()),
            target: String::new(),
        });
    }
    out.push(PathRow {
        label: "active log".into(),
        method: "new",
        source: Some(show(&src.lgat_path)),
        target: show(&o.log_dir.join(format!("{}_lgat", o.target_name))),
    });
    out.push(PathRow {
        label: "archives".into(),
        method: "none",
        source: Some(show(dir_of(&src.lgat_path))),
        target: show(&o.log_dir),
    });
    let lob = lob_base_dir(&src.entry).map_err(io::Error::other)?;
    out.push(tree_row("LOB", lob, &o.lob_dir, o.allow_clone)?);
    for d in ["java", "java_static"] {
        out.push(tree_row(
            if d == "java" {
                "Java SP"
            } else {
                "Java static"
            },
            Some(src.entry.vol_path.join(d)),
            &o.target_dir.join(d),
            o.allow_clone,
        )?);
    }
    out.push(PathRow {
        label: "databases.txt".into(),
        method: if o.replace { "replace" } else { "add" },
        source: Some(show(&o.source_dbtxt)),
        target: show(&o.target_dbtxt),
    });
    Ok(out)
}
