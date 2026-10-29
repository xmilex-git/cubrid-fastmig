//! What regeneration does with each system class: keep its slot, allocate it anew, create it, or drop it.
//!
//! Input: a system class name in the source (11.4; 11.2/11.3 lack one same-slot table) or target (develop) catalog,
//! and the source version. Output: its [`SystemClassRole`]
//! and, for a kept class, its develop name.
//! Source: decision workspace#482 1 (classification); names from the T1 11.4 DB and the develop catalog template
//! (49 system classes in 11.4, 74 in develop: 30 tables, 25 system views, 19 `INFORMATION_SCHEMA` views); R4.

use fastmig_ondisk::SourceVersion;

/// The role of one system class in regeneration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemClassRole {
    /// Original-data table: its class OID and rows stay in their slots, rewritten in the develop layout; the class
    /// takes the develop definition (and name, when renamed).
    SameSlot { develop_name: &'static str },
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

/// 11.4 name → develop name of the original-data tables (decision 1, first bullet).
pub const SAME_SLOT: [(&str, &str); 12] = [
    ("db_root", "db_root"),
    ("db_user", "_db_user"),
    ("db_password", "_db_password"),
    ("db_authorization", "_db_authorization"),
    ("db_trigger", "_db_trigger"),
    ("db_serial", "_db_serial"),
    ("_db_stored_procedure", "_db_stored_procedure"),
    ("_db_stored_procedure_args", "_db_stored_procedure_args"),
    ("_db_stored_procedure_code", "_db_stored_procedure_code"),
    ("_db_synonym", "_db_synonym"),
    ("_db_server", "_db_server"),
    ("db_ha_apply_info", "_db_ha_apply_info"),
];

/// Derived (11 ct tables, `_db_auth`) and static (`_db_data_type`, `_db_charset`, `_db_collation`, `dual`)
/// tables, same names in both lines.
pub const REALLOCATED: [&str; 16] = [
    "_db_class",
    "_db_attribute",
    "_db_domain",
    "_db_method",
    "_db_meth_sig",
    "_db_meth_arg",
    "_db_meth_file",
    "_db_query_spec",
    "_db_index",
    "_db_index_key",
    "_db_partition",
    "_db_auth",
    "_db_data_type",
    "_db_charset",
    "_db_collation",
    "dual",
];

/// Develop tables 11.4 lacks.
pub const NEW: [&str; 2] = ["_db_histogram", "_db_global_tran"];

/// 11.4 tables develop lacks.
pub const DROPPED: [&str; 1] = ["db_authorizations"];

/// Same-slot tables of [`SAME_SLOT`] a source version does not have: 11.2 and 11.3 have no
/// `_db_stored_procedure_code` (added with the stored procedure redesign, 1ea4a86e0 #5438; R8 § 2.11). Their develop
/// class comes from the template with an empty heap, like a [`SystemClassRole::New`] one.
pub fn absent_same_slot(version: SourceVersion) -> &'static [&'static str] {
    match version {
        SourceVersion::V11_2 | SourceVersion::V11_3 => &["_db_stored_procedure_code"],
        SourceVersion::V10_2
        | SourceVersion::V11_0
        | SourceVersion::V11_4
        | SourceVersion::Develop => &[],
    }
}

/// The [`SAME_SLOT`] pairs a source version has.
pub fn same_slot(
    version: SourceVersion,
) -> impl Iterator<Item = &'static (&'static str, &'static str)> {
    SAME_SLOT
        .iter()
        .filter(move |(s, _)| !absent_same_slot(version).contains(s))
}

/// Role of a system class of the source (11.4) catalog, by its name; `None` for a name this table does not know.
pub fn source_role(name: &str, is_view: bool) -> Option<SystemClassRole> {
    if is_view {
        return Some(SystemClassRole::View);
    }
    if let Some((_, d)) = SAME_SLOT.iter().find(|(s, _)| *s == name) {
        return Some(SystemClassRole::SameSlot { develop_name: d });
    }
    if REALLOCATED.contains(&name) {
        return Some(SystemClassRole::Reallocated);
    }
    DROPPED.contains(&name).then_some(SystemClassRole::Dropped)
}

/// Role of a system class of the target (develop) template, by its name, for a source of `version`; `None` for a
/// name this table does not know.
pub fn target_role(name: &str, is_view: bool, version: SourceVersion) -> Option<SystemClassRole> {
    if is_view {
        return Some(SystemClassRole::View);
    }
    if let Some((s, d)) = SAME_SLOT.iter().find(|(_, d)| *d == name) {
        if absent_same_slot(version).contains(s) {
            return Some(SystemClassRole::New);
        }
        return Some(SystemClassRole::SameSlot { develop_name: d });
    }
    if REALLOCATED.contains(&name) {
        return Some(SystemClassRole::Reallocated);
    }
    NEW.contains(&name).then_some(SystemClassRole::New)
}
