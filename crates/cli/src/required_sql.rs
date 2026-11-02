//! The SQL files a finished migrate leaves for the user, as the list of required follow-up steps in the order they
//! run on the target: printed at the end of migrate and kept in report.json.
//!
//! Input: the plan (routine, view and deferred index counts). Output: the steps; their text for stderr; their JSON; the
//! notes rows and the big box of a terminal (workspace#610 decision 9).
//! Order: PL/CSQL routines, then views (a view spec may call a routine), then deferred indexes, then statistics (taken
//! with every index in place). A file with no statement is left out but update-statistics.sql, which always runs unless `--no-statistics`
//! (workspace#604).
//! Source: workspace#599 (a view not recompiled fails a debug develop server's output check, unlike a deferred index,
//! which only makes a query slower), workspace#488 (PL/CSQL), workspace#486 (deferred indexes), workspace#482 7.

use std::path::Path;

use fastmig_plan::migration_plan::Plan;
use fastmig_progress::{Color, SqlNote, Style, big_text, panel};

/// One file to run, `csql -u dba <db> -i <dest>/fastmig/<file>`.
pub struct RequiredSql {
    pub file: &'static str,
    /// Statements the file holds for the plan's objects (routines, views, indexes); `None` for update-statistics.sql.
    pub objects: Option<usize>,
    /// The objects' plural noun.
    pub noun: &'static str,
    /// What the target does until the file runs.
    pub until_run: &'static str,
}

/// `statistics` false: `--no-statistics`, update-statistics.sql left out (workspace#604).
pub fn list(plan: &Plan, statistics: bool) -> Vec<RequiredSql> {
    let steps = [
        RequiredSql {
            file: "recompile-plcsql.sql",
            objects: Some(plan.plcsql_routines),
            noun: "PL/CSQL routines",
            until_run: "PL/CSQL routines run their copied compiled code; a call may fail with a load error",
        },
        RequiredSql {
            file: "recompile-views.sql",
            objects: Some(plan.recompiled_views),
            noun: "views",
            until_run: "a column of these views returns another domain than declared: a debug-build develop server \
                        aborts on the query (assert in qdata_check_output_column), 11.4 returns the value uncast",
        },
        RequiredSql {
            file: "deferred-indexes.sql",
            objects: Some(plan.deferred_indexes.len()),
            noun: "deferred indexes",
            until_run: "these function and filter indexes do not exist: queries that would use them are slower and a \
                        unique one checks no key",
        },
        RequiredSql {
            file: "update-statistics.sql",
            objects: None,
            noun: "statistics",
            until_run: "the optimizer plans with the statistics of the source or none",
        },
    ];
    steps
        .into_iter()
        .filter(|s| s.objects != Some(0) && (statistics || s.objects.is_some()))
        .collect()
}

/// The block migrate prints last on stderr.
pub fn text(steps: &[RequiredSql]) -> String {
    let mut s = String::from(
        "REQUIRED before the target is used: run as DBA in this order (csql -u dba <db> -i <dest>/fastmig/<file>)\n",
    );
    for (i, r) in steps.iter().enumerate() {
        let n = r.objects.map(|n| format!(" ({n})")).unwrap_or_default();
        s.push_str(&format!(
            "  {}. {}{n}: until it runs, {}\n",
            i + 1,
            r.file,
            r.until_run
        ));
    }
    s
}

/// The `required_sql` array of report.json.
pub fn json(steps: &[RequiredSql]) -> String {
    let items: Vec<String> = steps
        .iter()
        .map(|r| {
            format!(
                "{{\"file\": \"{}\", \"objects\": {}, \"until_run\": \"{}\"}}",
                r.file,
                r.objects.map_or("null".into(), |n| n.to_string()),
                r.until_run
            )
        })
        .collect();
    format!("[{}]", items.join(", "))
}

/// The notes rows of the progress screen: the files with objects, as `<objects> <noun> → <file>`.
pub fn notes(steps: &[RequiredSql]) -> Vec<SqlNote> {
    (steps.iter())
        .filter_map(|r| {
            Some(SqlNote {
                file: r.file.to_string(),
                objects: r.objects?,
                noun: r.noun.to_string(),
            })
        })
        .collect()
}

/// The box a terminal shows last: `ACTION REQUIRED` in big letters, then per file the command to run and what it
/// fixes; a green `no SQL required` box when there is none. `planned`: plan's list of what migrate will leave.
/// A command line wider than the box is written whole past the right edge ([`panel::boxed_whole`]).
/// `post`: the outcome lines of `--run-sql` and whether everything passed (workspace#611): passed, the box says the
/// files ran and shows the lines; failed, the lines join the box of commands under its list.
pub fn screen(
    st: &Style,
    steps: &[RequiredSql],
    db: &str,
    fastmig_dir: &Path,
    width: usize,
    planned: bool,
    post: Option<(&[String], bool)>,
) -> Vec<String> {
    if let Some((lines, true)) = post {
        let lines: Vec<String> = (lines.iter())
            .flat_map(|l| panel::wrap(l, width.saturating_sub(4)))
            .map(|l| st.paint(Color::Green, &l))
            .collect();
        return panel::boxed_whole(
            st,
            &format!("required SQL ({}): ran by --run-sql", steps.len()),
            &lines,
            Color::Green,
            width,
        );
    }
    if steps.is_empty() && post.is_none() {
        let what = if planned {
            "no SQL required after migrate"
        } else {
            "no SQL required: the target is ready"
        };
        return panel::boxed(
            st,
            "required SQL",
            &[st.strong(Color::Green, what)],
            Color::Green,
            width,
        );
    }
    let mut lines = Vec::new();
    if st.unicode {
        lines.extend(
            big_text::rows("action required")
                .iter()
                .map(|r| st.strong(Color::Yellow, r)),
        );
    } else {
        lines.push(st.strong(Color::Yellow, "*** ACTION REQUIRED ***"));
    }
    lines.push(String::new());
    lines.push(st.strong(
        Color::Bold,
        if planned {
            "after migrate, run as DBA on the target in this order before it is used:"
        } else {
            "run as DBA on the target in this order before it is used:"
        },
    ));
    for (i, r) in steps.iter().enumerate() {
        lines.push(String::new());
        let cmd = format!(
            "{}. csql -u dba {db} -i {}",
            i + 1,
            fastmig_dir.join(r.file).display()
        );
        lines.push(st.strong(Color::Yellow, &cmd));
        let n = r
            .objects
            .map(|n| format!("{n} {}: ", r.noun))
            .unwrap_or_default();
        let why = format!("{n}until it runs, {}", r.until_run);
        lines.extend(
            panel::wrap(&why, width.saturating_sub(7))
                .iter()
                .map(|l| st.paint(Color::Gray, &format!("   {l}"))),
        );
    }
    if let Some((post, _)) = post {
        lines.push(String::new());
        lines.extend(
            (post.iter())
                .flat_map(|l| panel::wrap(l, width.saturating_sub(4)))
                .map(|l| st.strong(Color::Red, &l)),
        );
    }
    panel::boxed_whole(
        st,
        &format!("required SQL ({})", steps.len()),
        &lines,
        Color::Yellow,
        width,
    )
}
