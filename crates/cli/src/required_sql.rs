//! The SQL files a finished migrate leaves for the user, as the list of required follow-up steps in the order they
//! run on the target: printed at the end of migrate and kept in report.json.
//!
//! Input: the plan (routine, view and deferred index counts). Output: the steps; their text for stderr; their JSON.
//! Order: PL/CSQL routines, then views (a view spec may call a routine), then deferred indexes, then statistics (taken
//! with every index in place). A file with no statement is left out but update-statistics.sql, which always runs.
//! Source: workspace#599 (a view not recompiled fails a debug develop server's output check, unlike a deferred index,
//! which only makes a query slower), workspace#488 (PL/CSQL), workspace#486 (deferred indexes), workspace#482 7.

use fastmig_plan::migration_plan::Plan;

/// One file to run, `csql -u dba <db> -i <dest>/fastmig/<file>`.
pub struct RequiredSql {
    pub file: &'static str,
    /// Statements the file holds for the plan's objects (routines, views, indexes); `None` for update-statistics.sql.
    pub objects: Option<usize>,
    /// What the target does until the file runs.
    pub until_run: &'static str,
}

pub fn list(plan: &Plan) -> Vec<RequiredSql> {
    let steps = [
        RequiredSql {
            file: "recompile-plcsql.sql",
            objects: Some(plan.plcsql_routines),
            until_run: "PL/CSQL routines run their copied compiled code; a call may fail with a load error",
        },
        RequiredSql {
            file: "recompile-views.sql",
            objects: Some(plan.recompiled_views),
            until_run: "a column of these views returns another domain than declared: a debug-build develop server \
                        aborts on the query (assert in qdata_check_output_column), 11.4 returns the value uncast",
        },
        RequiredSql {
            file: "deferred-indexes.sql",
            objects: Some(plan.deferred_indexes.len()),
            until_run: "these function and filter indexes do not exist: queries that would use them are slower and a \
                        unique one checks no key",
        },
        RequiredSql {
            file: "update-statistics.sql",
            objects: None,
            until_run: "the optimizer plans with the statistics of the source or none",
        },
    ];
    steps.into_iter().filter(|s| s.objects != Some(0)).collect()
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
