//! The system tables catalog regeneration knows, by name in the source and in the develop catalog.
//!
//! Input: a system class name. Output: its [`SystemTable`]; or a table's name in either catalog.
//! Source: names from the T1 11.4 DB and the develop catalog template (R4); 10.2..11.3 use the 11.4 names
//! (R8 § 2.11); develop renamed the `db_*` original-data tables to `_db_*` (CBRD-25862, develop 8ba543178).

use fastmig_ondisk::VersionLine;

/// A system table (not a view) of the source or the develop catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SystemTable {
    DbRoot,
    DbUser,
    DbPassword,
    DbAuthorization,
    DbTrigger,
    DbSerial,
    DbStoredProcedure,
    DbStoredProcedureArgs,
    DbStoredProcedureCode,
    DbSynonym,
    DbServer,
    DbHaApplyInfo,
    DbClass,
    DbAttribute,
    DbDomain,
    DbMethod,
    DbMethSig,
    DbMethArg,
    DbMethFile,
    DbQuerySpec,
    DbIndex,
    DbIndexKey,
    DbPartition,
    DbAuth,
    DbDataType,
    DbCharset,
    DbCollation,
    Dual,
    /// develop only.
    DbHistogram,
    /// develop only.
    DbGlobalTran,
    /// 10.2..11.4 only.
    DbAuthorizations,
}

impl SystemTable {
    /// (name in a 10.2..11.4 catalog, name in the develop catalog); `None` where the table does not exist.
    const fn names(self) -> (Option<&'static str>, Option<&'static str>) {
        use SystemTable::*;
        match self {
            DbRoot => both("db_root"),
            DbUser => (Some("db_user"), Some("_db_user")),
            DbPassword => (Some("db_password"), Some("_db_password")),
            DbAuthorization => (Some("db_authorization"), Some("_db_authorization")),
            DbTrigger => (Some("db_trigger"), Some("_db_trigger")),
            DbSerial => (Some("db_serial"), Some("_db_serial")),
            DbStoredProcedure => both("_db_stored_procedure"),
            DbStoredProcedureArgs => both("_db_stored_procedure_args"),
            DbStoredProcedureCode => both("_db_stored_procedure_code"),
            DbSynonym => both("_db_synonym"),
            DbServer => both("_db_server"),
            DbHaApplyInfo => (Some("db_ha_apply_info"), Some("_db_ha_apply_info")),
            DbClass => both("_db_class"),
            DbAttribute => both("_db_attribute"),
            DbDomain => both("_db_domain"),
            DbMethod => both("_db_method"),
            DbMethSig => both("_db_meth_sig"),
            DbMethArg => both("_db_meth_arg"),
            DbMethFile => both("_db_meth_file"),
            DbQuerySpec => both("_db_query_spec"),
            DbIndex => both("_db_index"),
            DbIndexKey => both("_db_index_key"),
            DbPartition => both("_db_partition"),
            DbAuth => both("_db_auth"),
            DbDataType => both("_db_data_type"),
            DbCharset => both("_db_charset"),
            DbCollation => both("_db_collation"),
            Dual => both("dual"),
            DbHistogram => (None, Some("_db_histogram")),
            DbGlobalTran => (None, Some("_db_global_tran")),
            DbAuthorizations => (Some("db_authorizations"), None),
        }
    }

    /// Name in a 10.2..11.4 catalog; `None` for a develop-only table.
    pub const fn source_name(self) -> Option<&'static str> {
        self.names().0
    }

    /// Name in the develop catalog; `None` for a table develop dropped.
    pub const fn develop_name(self) -> Option<&'static str> {
        self.names().1
    }

    /// Name in a catalog of `line`; `None` where that catalog has no such table.
    pub const fn name_in(self, line: VersionLine) -> Option<&'static str> {
        match line {
            VersionLine::V10_2 | VersionLine::V11_4 => self.source_name(),
            VersionLine::Develop => self.develop_name(),
        }
    }

    /// Develop name of a table develop has: every table regeneration writes rows into or reads from the template.
    pub const fn name(self) -> &'static str {
        match self.develop_name() {
            Some(n) => n,
            None => panic!("a table develop dropped has no develop name"),
        }
    }
}

/// A table both catalogs name alike.
const fn both(s: &'static str) -> (Option<&'static str>, Option<&'static str>) {
    (Some(s), Some(s))
}

impl std::fmt::Display for SystemTable {
    /// The develop name, or the source name of a table develop dropped.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            self.develop_name()
                .or(self.source_name())
                .unwrap_or_default(),
        )
    }
}
