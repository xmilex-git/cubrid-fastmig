//! What a finished migrate shows on a terminal instead of report.json: a result box (time against the start
//! estimate, the check's outcome, where the target is registered, the report and the run log, the source and target
//! paths; each phase's time is in the phases box above it), then the required SQL box.
//!
//! Input: the finished progress, the check's summary line (`null` without `--check`), the required SQL steps and the
//! paths. Output: the text for stdout. Without a terminal on stdout migrate prints report.json as before.
//! Source: workspace#610 ("바뀌지 않는 것": a terminal gets the summary box, the JSON stays in report.json; decision 9);
//! workspace#612 decision 6 (the paths, whole).

use std::path::Path;

use fastmig_progress::panel::boxed_whole;
use fastmig_progress::{Color, PathRow, Progress, Stream, Style, path_panel, style::term_size};

use crate::required_sql::{self, RequiredSql};

/// `secs` as the phases box prints it: `0.41 s` under a minute, `m:ss` above.
fn secs(s: f64) -> String {
    if s < 60.0 {
        format!("{s:.2} s")
    } else {
        fastmig_progress::clock::duration(s)
    }
}

/// The value of `"key":<n>` in the verifier's summary line.
pub fn count(summary: &str, key: &str) -> Option<u64> {
    let at = summary.find(&format!("\"{key}\":"))? + key.len() + 3;
    let digits: String = summary[at..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// `post`: the `--run-sql` outcome for the SQL box (`required_sql::screen`, workspace#611).
#[allow(clippy::too_many_arguments)]
pub fn text(
    st: &Style,
    p: &Progress,
    summary: &str,
    steps: &[RequiredSql],
    db: &str,
    fastmig_dir: &Path,
    dbtxt: &Path,
    post: Option<(&[String], bool)>,
    // the migration's own time, taken before --run-sql ran (workspace#611)
    took: f64,
    paths: &[PathRow],
) -> String {
    let width = term_size(Stream::Stdout).0.saturating_sub(1).max(40);
    let mut lines = Vec::new();
    let est = p.start_estimate().map_or(String::new(), |e| {
        format!(" (estimated at start ~{})", secs(e))
    });
    lines.push(format!(
        "took {}{est}",
        st.strong(Color::Green, &secs(took))
    ));
    lines.push(match count(summary, "migration") {
        Some(0) => format!(
            "check  {}, {} pre-existing, {} resolved",
            st.strong(Color::Green, "0 migration defects"),
            count(summary, "pre_existing").unwrap_or(0),
            count(summary, "resolved").unwrap_or(0)
        ),
        Some(n) => st.strong(Color::Red, &format!("check  {n} migration defects")),
        None => st.paint(
            Color::Yellow,
            "check  not run (pass --check); finalize needs a passing check",
        ),
    });
    lines.push(format!(
        "target {} registered in {}",
        st.strong(Color::Bold, db),
        dbtxt.display()
    ));
    lines.push(format!(
        "report {}",
        fastmig_dir.join("report.json").display()
    ));
    if let Some(l) = p.log_path() {
        lines.push(format!("log    {}", l.display()));
    }
    lines.push(String::new());
    lines.extend(path_panel::lines(st, paths, width - 4));
    let mut out = boxed_whole(
        st,
        "cubrid-fastmig migrate: done",
        &lines,
        Color::Green,
        width,
    );
    out.extend(required_sql::screen(
        st,
        steps,
        db,
        fastmig_dir,
        width,
        false,
        post,
    ));
    out.iter().map(|l| format!("{l}\n")).collect()
}
