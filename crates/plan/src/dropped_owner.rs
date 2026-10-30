//! Stored procedures whose owner user was dropped: 10.2..11.3 `DROP USER` does not check procedures (11.4 added
//! `_db_stored_procedure` to the owned-object list), so a procedure can keep a reference to a deleted `db_user` row.
//! Its owner-qualified name cannot be formed, and the engine's official path cannot move the database either
//! (10.2.18 and 11.3.6 unloaddb exit 1 without an objects file), so the plan refuses with the fix on the source.
//! A class, trigger, serial, server or synonym cannot lose its owner this way: `DROP USER` refuses while the user owns
//! one, and `DELETE FROM db_user` is not authorized (10.2.18, 11.3.6, 11.4.6; workspace#556).
//!
//! Input: the source's `_db_stored_procedure` and `db_user` rows. Output: one refusal per such procedure.
//! Source: engine `au_drop_user` (release/10.2 authenticate.c:3541 `_db_class`, `db_trigger`, `db_serial`; release/11.3
//! adds `_db_server`, `_db_synonym`; release/11.4 authenticate_access_user.cpp:1096-1104 adds `_db_stored_procedure`);
//! workspace#556 probe (R10 workspace#511 left it open).

use std::collections::HashSet;

use fastmig_catalog::class_rows::Row;
use fastmig_catalog::system_rows::stored_procedure;
use fastmig_ondisk::Oid;
use fastmig_ondisk::value::Value;

pub fn refusals(procedures: &[Row], users: &[Row]) -> Vec<String> {
    let live: HashSet<Oid> = users.iter().map(|u| u.oid).collect();
    procedures
        .iter()
        .filter(|r| matches!(stored_procedure::owner(&r.values), Some(Value::Object(o)) if !live.contains(o)))
        .map(|r| {
            format!(
                "stored procedure {}: its owner user was dropped, so it has no owner-qualified name (the engine's \
                 unloaddb fails on it too); fix: on the source, drop the procedure and create it again as an existing \
                 user, then migrate again (workspace#556)",
                String::from_utf8_lossy(stored_procedure::sp_name(&r.values).unwrap_or_default())
            )
        })
        .collect()
}
