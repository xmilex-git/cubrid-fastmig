//! What regeneration does with each system class: keep its slot, allocate it anew, create it, or drop it.
//!
//! Input: a system class name in the source (11.4; 11.2/11.3 lack one same-slot table, 10.2 three and `dual`) or
//! target (develop or 11.4) catalog, the source version and the target line. Output: its [`SystemClassRole`]
//! and, for a kept class, its name in the target.
//! Source: decision workspace#482 1 (classification); names from the T1 11.4 DB and the develop catalog template
//! (49 system classes in 11.4, 74 in develop: 30 tables, 25 system views, 19 `INFORMATION_SCHEMA` views); R4. An 11.4
//! target has every 11.4 table: none is new or dropped, and `db_authorizations` (class methods only, no rows,
//! release/11.4 `authenticate_context.cpp` `au_install`) takes the template class in its slot (workspace#552).

use fastmig_ondisk::{SourceVersion, VersionLine};

use crate::system_table::SystemTable;

/// The role of one system class in regeneration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemClassRole {
    /// Original-data table: its class OID and rows stay in their slots, rewritten in the develop layout; the class
    /// takes the develop definition (and name, when renamed).
    SameSlot { table: SystemTable },
    /// Derived or static table: old heap freed, new heap allocated and filled (ct tables from the class objects,
    /// `_db_auth` from the grants, `_db_data_type` and `dual` from the template, `_db_charset`/`_db_collation`
    /// from the source rows).
    Reallocated,
    /// A table develop has and 11.4 does not: new class object from the template, empty heap.
    New,
    /// A table 11.4 has and develop does not: class object slot deleted, heap freed.
    Dropped,
    /// A system view: 11.4's are deleted, develop's come from the template in new slots.
    View,
}

/// The original-data tables (decision 1, first bullet); develop renamed some ([`SystemTable::develop_name`]).
pub const SAME_SLOT: [SystemTable; 12] = [
    SystemTable::DbRoot,
    SystemTable::DbUser,
    SystemTable::DbPassword,
    SystemTable::DbAuthorization,
    SystemTable::DbTrigger,
    SystemTable::DbSerial,
    SystemTable::DbStoredProcedure,
    SystemTable::DbStoredProcedureArgs,
    SystemTable::DbStoredProcedureCode,
    SystemTable::DbSynonym,
    SystemTable::DbServer,
    SystemTable::DbHaApplyInfo,
];

/// Derived (11 ct tables, `_db_auth`) and static (`_db_data_type`, `_db_charset`, `_db_collation`, `dual`)
/// tables, same names in both lines.
pub const REALLOCATED: [SystemTable; 16] = [
    SystemTable::DbClass,
    SystemTable::DbAttribute,
    SystemTable::DbDomain,
    SystemTable::DbMethod,
    SystemTable::DbMethSig,
    SystemTable::DbMethArg,
    SystemTable::DbMethFile,
    SystemTable::DbQuerySpec,
    SystemTable::DbIndex,
    SystemTable::DbIndexKey,
    SystemTable::DbPartition,
    SystemTable::DbAuth,
    SystemTable::DbDataType,
    SystemTable::DbCharset,
    SystemTable::DbCollation,
    SystemTable::Dual,
];

/// Develop tables 11.4 lacks: new on a develop target.
pub const NEW: [SystemTable; 2] = [SystemTable::DbHistogram, SystemTable::DbGlobalTran];

/// 11.4 tables develop lacks: dropped on a develop target, reallocated on an 11.4 one.
pub const DROPPED: [SystemTable; 1] = [SystemTable::DbAuthorizations];

/// Tables of [`SAME_SLOT`] and [`REALLOCATED`] a source version does not have: 10.2 to 11.3 have no
/// `_db_stored_procedure_code` (added with the stored procedure redesign, 1ea4a86e0 #5438; R8 § 2.11). Their develop
/// class comes from the template in a new slot, like a [`SystemClassRole::New`] one, with an empty heap or, for
/// `dual`, the template row ([`crate::static_rows`]).
pub fn absent_tables(version: SourceVersion) -> &'static [SystemTable] {
    match version {
        SourceVersion::V11_2 | SourceVersion::V11_3 => &[SystemTable::DbStoredProcedureCode],
        // 10.2 has neither synonyms nor DBLink servers nor `dual` (a system class since 11.0, R10 § 5); the T4 10.2
        // root heap has none of the four (workspace#544, workspace#549)
        SourceVersion::V10_2 => &[
            SystemTable::DbStoredProcedureCode,
            SystemTable::DbSynonym,
            SystemTable::DbServer,
            SystemTable::Dual,
        ],
        SourceVersion::V11_0 | SourceVersion::V11_4 | SourceVersion::Develop => &[],
    }
}

/// The [`SAME_SLOT`] tables a source version has.
pub fn same_slot(version: SourceVersion) -> impl Iterator<Item = SystemTable> {
    SAME_SLOT
        .into_iter()
        .filter(move |t| !absent_tables(version).contains(t))
}

/// The 11.4 tables develop lacks, as a target of `to` treats them: dropped on develop, reallocated on 11.4.
fn old_tables_role(to: VersionLine) -> SystemClassRole {
    match to {
        VersionLine::Develop => SystemClassRole::Dropped,
        VersionLine::V10_2 | VersionLine::V11_4 => SystemClassRole::Reallocated,
    }
}

/// Role of a system class of the source (10.2..11.4) catalog, by its name, for a target of line `to`; `None` for a
/// name this table does not know.
pub fn source_role(name: &str, is_view: bool, to: VersionLine) -> Option<SystemClassRole> {
    if is_view {
        return Some(SystemClassRole::View);
    }
    let named = |t: &SystemTable| t.source_name() == Some(name);
    if let Some(&table) = SAME_SLOT.iter().find(|t| named(t)) {
        return Some(SystemClassRole::SameSlot { table });
    }
    if REALLOCATED.iter().any(named) {
        return Some(SystemClassRole::Reallocated);
    }
    DROPPED.iter().any(named).then(|| old_tables_role(to))
}

/// Role of a system class of the target template (line `to`), by its name, for a source of `version`; `None` for a
/// name this table does not know.
pub fn target_role(
    name: &str,
    is_view: bool,
    version: SourceVersion,
    to: VersionLine,
) -> Option<SystemClassRole> {
    if is_view {
        return Some(SystemClassRole::View);
    }
    let named = |t: &SystemTable| t.name_in(to) == Some(name);
    if absent_tables(version).iter().any(named) {
        return Some(SystemClassRole::New);
    }
    if let Some(&table) = SAME_SLOT.iter().find(|t| named(t)) {
        return Some(SystemClassRole::SameSlot { table });
    }
    if REALLOCATED.iter().any(named) {
        return Some(SystemClassRole::Reallocated);
    }
    if DROPPED.iter().any(named) {
        return Some(old_tables_role(to));
    }
    NEW.iter().any(named).then_some(SystemClassRole::New)
}
