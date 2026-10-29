//! What regeneration does with each system class: keep its slot, allocate it anew, create it, or drop it.
//!
//! Input: a system class name in the source (11.4; 11.2/11.3 lack one same-slot table) or target (develop) catalog,
//! and the source version. Output: its [`SystemClassRole`]
//! and, for a kept class, its develop name.
//! Source: decision workspace#482 1 (classification); names from the T1 11.4 DB and the develop catalog template
//! (49 system classes in 11.4, 74 in develop: 30 tables, 25 system views, 19 `INFORMATION_SCHEMA` views); R4.

use fastmig_ondisk::SourceVersion;

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

/// Develop tables 11.4 lacks.
pub const NEW: [SystemTable; 2] = [SystemTable::DbHistogram, SystemTable::DbGlobalTran];

/// 11.4 tables develop lacks.
pub const DROPPED: [SystemTable; 1] = [SystemTable::DbAuthorizations];

/// Same-slot tables of [`SAME_SLOT`] a source version does not have: 11.2 and 11.3 have no
/// `_db_stored_procedure_code` (added with the stored procedure redesign, 1ea4a86e0 #5438; R8 § 2.11). Their develop
/// class comes from the template with an empty heap, like a [`SystemClassRole::New`] one.
pub fn absent_same_slot(version: SourceVersion) -> &'static [SystemTable] {
    match version {
        SourceVersion::V11_2 | SourceVersion::V11_3 => &[SystemTable::DbStoredProcedureCode],
        SourceVersion::V10_2
        | SourceVersion::V11_0
        | SourceVersion::V11_4
        | SourceVersion::Develop => &[],
    }
}

/// The [`SAME_SLOT`] tables a source version has.
pub fn same_slot(version: SourceVersion) -> impl Iterator<Item = SystemTable> {
    SAME_SLOT
        .into_iter()
        .filter(move |t| !absent_same_slot(version).contains(t))
}

/// Role of a system class of the source (11.4) catalog, by its name; `None` for a name this table does not know.
pub fn source_role(name: &str, is_view: bool) -> Option<SystemClassRole> {
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
    DROPPED
        .iter()
        .any(named)
        .then_some(SystemClassRole::Dropped)
}

/// Role of a system class of the target (develop) template, by its name, for a source of `version`; `None` for a
/// name this table does not know.
pub fn target_role(name: &str, is_view: bool, version: SourceVersion) -> Option<SystemClassRole> {
    if is_view {
        return Some(SystemClassRole::View);
    }
    let named = |t: &SystemTable| t.develop_name() == Some(name);
    if let Some(&table) = SAME_SLOT.iter().find(|t| named(t)) {
        if absent_same_slot(version).contains(&table) {
            return Some(SystemClassRole::New);
        }
        return Some(SystemClassRole::SameSlot { table });
    }
    if REALLOCATED.iter().any(named) {
        return Some(SystemClassRole::Reallocated);
    }
    NEW.iter().any(named).then_some(SystemClassRole::New)
}
