//! The EXECUTE grants a target needs: a source before 11.4 has no `GRANT EXECUTE` (11.4 added it to the grammar), so
//! a view or trigger calling another owner's stored procedure fails on develop with "EXECUTE authorization failure".
//! The tool grants nothing, as the engine's official path (unloaddb → loaddb); the plan warns and writes one
//! `GRANT EXECUTE` per (procedure, caller) into `<dest>/fastmig/post-migration-fixes.sql`.
//!
//! Input: the other-owner procedure calls (GLOSSARY) `owner_qualification` found; callers in the DBA group and calls
//! of a caller's own procedures are not among them. Output: [`ExecuteGrants`].
//! Source: user decision G11 workspace#545 (2026-10-30: warning and GRANT statements, no grant made); T6 workspace#516
//! and G8 workspace#517 (before the GRANT the view and trigger fail, after it they run); develop's grammar takes
//! `EXECUTE ON PROCEDURE` for functions too (`csql_grammar.y` authorized_execute_procedure_cmd), and
//! `[owner.sp]` names a procedure there as in the stored texts (workspace#545 probe).

use std::collections::BTreeSet;

use fastmig_catalog::owner_qualification::OtherOwnerProcedureCall;

/// How many calls the warning names.
const EXAMPLES: usize = 3;

#[derive(Default)]
pub struct ExecuteGrants {
    pub warning: Option<String>,
    /// The post-migration-fixes.sql section; empty when no call needs a grant.
    pub fix_sql: String,
}

pub fn execute_grants(calls: &[OtherOwnerProcedureCall]) -> ExecuteGrants {
    let grants: BTreeSet<(&str, &str)> = calls
        .iter()
        .map(|c| (c.procedure.as_str(), c.caller.as_str()))
        .collect();
    if grants.is_empty() {
        return ExecuteGrants::default();
    }
    let objects: BTreeSet<&str> = calls.iter().map(|c| c.object.as_str()).collect();
    let examples: Vec<String> = calls
        .iter()
        .take(EXAMPLES)
        .map(|c| format!("{} calls {}", c.object, c.procedure))
        .collect();
    let warning = format!(
        "{} views and triggers call a stored procedure of another owner ({}{}); develop runs one only with EXECUTE, \
         which a source before 11.4 cannot grant; the tool grants nothing: run the {} GRANT EXECUTE statements of \
         fastmig/post-migration-fixes.sql on the target (workspace#545)",
        objects.len(),
        examples.join(", "),
        if calls.len() > EXAMPLES { ", ..." } else { "" },
        grants.len()
    );
    let mut fix_sql = String::from(
        "-- Stored procedures views and triggers call across owners: develop runs them only with EXECUTE (workspace#545).\n",
    );
    for (procedure, caller) in grants {
        fix_sql.push_str(&format!(
            "GRANT EXECUTE ON PROCEDURE [{procedure}] TO [{caller}];\n"
        ));
    }
    ExecuteGrants {
        warning: Some(warning),
        fix_sql,
    }
}
