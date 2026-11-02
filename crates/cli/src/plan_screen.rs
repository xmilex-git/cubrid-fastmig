//! `plan` on a terminal: the plan in boxes instead of the table — the header (source → target), volumes, classes,
//! target space, resources, notes (the warnings grouped by the SQL file that fixes them), refusals, and the required
//! SQL migrate will leave, in the box migrate ends with.
//!
//! Input: the plan, its target space and resource table (or why they were not estimated), the storage kinds, the
//! [`Style`]. Output: the text for stdout. Without a terminal on stdout, or with `--json`, plan prints as before.
//! Paths are cut in the middle and long lists cut to a line (`plan --json` has them whole); warnings and refusals are
//! wrapped, never cut.
//! Source: workspace#610 decisions 1 (나), 5, 7, 9.

use std::collections::BTreeSet;
use std::path::Path;

use fastmig_plan::migration_plan::VolumePlan;
use fastmig_plan::version_pair::line_name;
use fastmig_plan::{HeapTreatment, PageTreatment, Plan};
use fastmig_progress::panel::{boxed, cut_middle, wrap};
use fastmig_progress::style::term_size;
use fastmig_progress::{Color, Stream, Style, big_text, unit};

use crate::required_sql::{self, RequiredSql};
use crate::target_space::TargetSpace;

/// Volume rows shown before the rest is counted.
const VOLUME_ROWS: usize = 12;
/// Other warnings shown one per line before the rest is counted.
const NOTE_ROWS: usize = 8;

pub struct PlanScreen<'a> {
    pub plan: &'a Plan,
    pub db: &'a str,
    pub target_name: &'a str,
    pub target_dir: &'a Path,
    /// The target space and the resource table's text, or why they were not estimated.
    pub estimate: Result<(&'a TargetSpace, String), &'a str>,
    /// The storage line (`storage: source nvme (nvme0n1), ...`).
    pub storage: String,
}

impl PlanScreen<'_> {
    pub fn text(&self, st: &Style) -> String {
        let (cols, rows) = term_size(Stream::Stdout);
        let width = cols.saturating_sub(1).max(40);
        let inner = width - 4;
        let p = self.plan;
        let steps = required_sql::list(p, true);
        let mut out = self.header(st, width, cols >= 100 && rows >= 40);
        out.extend(boxed(
            st,
            &format!("volumes ({})", p.volumes.len()),
            &self.volumes(st, inner),
            Color::Blue,
            width,
        ));
        out.extend(boxed(
            st,
            "classes",
            &self.classes(st, inner),
            Color::Blue,
            width,
        ));
        let (space, resources) = match &self.estimate {
            Ok((t, table)) => (self.space(t, inner), table_lines(table, inner)),
            Err(e) => (wrap(&format!("not estimated: {e}"), inner), Vec::new()),
        };
        out.extend(boxed(st, "target space", &space, Color::Magenta, width));
        let mut res = resources;
        res.extend(wrap(&self.storage, inner));
        out.extend(boxed(st, "resources", &res, Color::Magenta, width));
        out.extend(boxed(
            st,
            &format!("notes ({})", p.warnings.len()),
            &self.notes(st, &steps, inner),
            Color::Yellow,
            width,
        ));
        if !p.refusals.is_empty() {
            let lines: Vec<String> = (p.refusals.iter())
                .flat_map(|r| wrap(r, inner))
                .map(|l| st.paint(Color::Red, &l))
                .collect();
            out.extend(boxed(
                st,
                &format!("refusals ({}): migrate would stop here", p.refusals.len()),
                &lines,
                Color::Red,
                width,
            ));
        }
        out.extend(required_sql::screen(
            st,
            &steps,
            self.target_name,
            &self.target_dir.join("fastmig"),
            width,
            true,
        ));
        out.iter().map(|l| format!("{l}\n")).collect()
    }

    fn header(&self, st: &Style, width: usize, logo: bool) -> Vec<String> {
        let p = self.plan;
        let mut lines = Vec::new();
        if logo && st.unicode {
            let [a, b, c] = big_text::rows("cubrid-fastmig");
            lines.push(st.strong(Color::Cyan, &format!(" {a}")));
            lines.push(st.strong(Color::Cyan, &format!(" {b}")));
            lines.push(st.strong(Color::Blue, &format!(" {c}")));
            lines.push(String::new());
        }
        lines.push(format!(
            "{} {}  {}  {} {}",
            st.strong(Color::Bold, self.db),
            st.paint(Color::Gray, &format!("({})", p.source_version.name())),
            st.paint(Color::Yellow, st.glyphs.arrow),
            st.strong(Color::Bold, self.target_name),
            st.paint(
                Color::Gray,
                &format!("({})", line_name(p.version_pair.target))
            )
        ));
        let bytes: u64 = p.volumes.iter().map(|v| v.bytes).sum();
        let verdict = if p.refusals.is_empty() {
            st.strong(Color::Green, "migrate can run")
        } else {
            st.strong(
                Color::Red,
                &format!("refused: see refusals ({}) below", p.refusals.len()),
            )
        };
        lines.push(format!(
            "{} volumes, {}   {verdict}",
            p.volumes.len(),
            unit::bytes(bytes as f64)
        ));
        boxed(st, "cubrid-fastmig plan", &lines, Color::Cyan, width)
    }

    fn volumes(&self, st: &Style, inner: usize) -> Vec<String> {
        let p = self.plan;
        // id 4, method 5, size 9 and the spaces: about 22; the rest split between the two paths
        let path_w = inner.saturating_sub(26) / 2;
        // every source in one directory and every target in one: the directories once, then the file names
        let one_dir = |f: fn(&VolumePlan) -> &Path| {
            let dirs: BTreeSet<&Path> = p.volumes.iter().filter_map(|v| f(v).parent()).collect();
            (dirs.len() == 1).then(|| dirs.into_iter().next()).flatten()
        };
        let dirs = one_dir(|v| &v.path).zip(one_dir(|v| &v.target));
        let mut out = Vec::new();
        if let Some((from, to)) = dirs {
            out.push(format!(
                "from {}",
                cut_middle(st, &from.display().to_string(), inner - 5)
            ));
            out.push(format!(
                "to   {}",
                cut_middle(st, &to.display().to_string(), inner - 5)
            ));
        }
        let shown = |path: &Path| match (dirs, path.file_name()) {
            (Some(_), Some(n)) => n.to_string_lossy().into_owned(),
            _ => cut_middle(st, &path.display().to_string(), path_w),
        };
        out.extend((p.volumes.iter().take(VOLUME_ROWS)).map(|v| {
            let m = v.copy_method.name();
            format!(
                "{:>4} {} {:>9}  {} {} {}",
                v.volid,
                st.paint(
                    if m == "clone" {
                        Color::Green
                    } else {
                        Color::Cyan
                    },
                    &format!("{m:<5}")
                ),
                unit::bytes(v.bytes as f64),
                shown(&v.path),
                st.glyphs.to,
                shown(&v.target)
            )
        }));
        if p.volumes.len() > VOLUME_ROWS {
            out.push(st.paint(
                Color::Gray,
                &format!(
                    "+{} more volumes (plan --json lists them)",
                    p.volumes.len() - VOLUME_ROWS
                ),
            ));
        }
        out
    }

    fn classes(&self, st: &Style, inner: usize) -> Vec<String> {
        let p = self.plan;
        let count = |h: HeapTreatment| p.classes.iter().filter(|c| c.heap == h).count();
        let list = |label: &str, names: Vec<&str>| {
            let s = format!("{label} ({}): {}", names.len(), names.join(", "));
            if s.chars().count() > inner {
                let cut: String = s.chars().take(inner.saturating_sub(16)).collect();
                format!("{cut} {}", st.paint(Color::Gray, "(plan --json)"))
            } else {
                s
            }
        };
        vec![
            format!(
                "heaps: {} patch, {} transform, {} freed, {} without heap",
                count(HeapTreatment::InPlace(PageTreatment::Patch)),
                count(HeapTreatment::InPlace(PageTreatment::Transform)),
                count(HeapTreatment::Freed),
                count(HeapTreatment::None)
            ),
            list("re-encoded classes", p.re_encoded().collect()),
            list(
                "deferred indexes",
                p.deferred_indexes.iter().map(String::as_str).collect(),
            ),
            list(
                "system name rewrites",
                p.system_name_rewrites.iter().map(String::as_str).collect(),
            ),
            format!(
                "LOB: {} referenced files ({}), {} orphan files ({}), {} outside the base",
                p.lob.referenced.len(),
                unit::bytes(p.lob.referenced_bytes() as f64),
                p.lob.orphans.len(),
                unit::bytes(p.lob.orphan_bytes() as f64),
                p.lob.outside_base.len()
            ),
        ]
    }

    fn space(&self, t: &TargetSpace, inner: usize) -> Vec<String> {
        let p = self.plan;
        let b = |n: u64| unit::bytes(n as f64);
        let mut out = vec![format!(
            "needs {} free, {} free",
            b(t.needed()),
            b(p.available_bytes)
        )];
        out.extend(wrap(
            &format!(
                "taken at the start: {} byte-copied volumes, {} new log, {} cloned pages unshared ({} sectors listed, {} short), {} reserved (upper bound of the rest)",
                b(t.copy_bytes),
                b(t.log_bytes),
                b(t.unshare_bytes),
                t.sectors.len(),
                t.short_sectors,
                b(t.extra_bytes)
            ),
            inner,
        ));
        out.push(format!(
            "unreserved writes up to {}: keep that much free of other writers during the run",
            b(t.extra_bytes)
        ));
        out
    }

    /// The SQL files with objects, then the other warnings, each wrapped.
    fn notes(&self, st: &Style, steps: &[RequiredSql], inner: usize) -> Vec<String> {
        let p = self.plan;
        let mut out: Vec<String> = (steps.iter())
            .filter_map(|r| {
                Some(format!(
                    "{} {} {} {} {}",
                    st.paint(Color::Yellow, st.glyphs.warning),
                    r.objects.filter(|&n| n > 0)?,
                    r.noun,
                    st.glyphs.to,
                    st.strong(Color::Yellow, r.file)
                ))
            })
            .collect();
        let others: Vec<&String> = (p.warnings.iter())
            .filter(|w| !steps.iter().any(|r| w.contains(r.file)))
            .collect();
        for w in others.iter().take(NOTE_ROWS) {
            out.extend(
                wrap(&format!("{} {w}", st.glyphs.dot), inner)
                    .into_iter()
                    .map(|l| st.paint(Color::Gray, &l)),
            );
        }
        if others.len() > NOTE_ROWS {
            out.push(st.paint(
                Color::Gray,
                &format!(
                    "+{} more (plan --json lists every warning)",
                    others.len() - NOTE_ROWS
                ),
            ));
        }
        out
    }
}

/// The resource table's lines (`resource_estimate::ResourceEstimate::table`) wrapped to the box.
fn table_lines(table: &str, inner: usize) -> Vec<String> {
    table.lines().flat_map(|l| wrap(l.trim(), inner)).collect()
}
