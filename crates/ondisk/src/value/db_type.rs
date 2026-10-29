//! Engine type ids (`DB_TYPE`) and which of them sit in a record's fixed area, per version.
//!
//! Input: a type id from a packed domain, a class object or a set header. Output: [`DbType`], and through
//! [`crate::value::Domain::disk_size`] the fixed disk size or "variable".
//! Source: engine `src/compat/dbtype_def.h` `enum db_type` (release/11.4 @46212adcd :694-736, develop @5b1867174
//! :746-794 — same numbers, develop renames 26/27 `DB_TYPE_NCHAR_DEPRECATED`/`DB_TYPE_VARNCHAR_DEPRECATED`);
//! `src/object/object_primitive.c` `tp_*` (`variable_p`, `disksize`); R2 § 2.

use crate::VersionLine;

/// Engine `DB_TYPE`. Only the ids that can appear on disk are listed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum DbType {
    Null = 0,
    Integer = 1,
    Float = 2,
    Double = 3,
    /// Engine `DB_TYPE_STRING` = `DB_TYPE_VARCHAR`.
    Varchar = 4,
    Object = 5,
    Set = 6,
    Multiset = 7,
    Sequence = 8,
    Elo = 9,
    Time = 10,
    Timestamp = 11,
    Date = 12,
    Monetary = 13,
    Variable = 14,
    Sub = 15,
    Short = 18,
    Vobj = 19,
    Oid = 20,
    Numeric = 22,
    Bit = 23,
    Varbit = 24,
    Char = 25,
    /// 11.4 only; develop has no NCHAR (CBRD-24156 #6420) and the map turns it into CHAR (workspace#482).
    Nchar = 26,
    /// 11.4 only; becomes VARCHAR with the same bytes (workspace#482).
    Varnchar = 27,
    Resultset = 28,
    Midxkey = 29,
    Table = 30,
    Bigint = 31,
    Datetime = 32,
    Blob = 33,
    Clob = 34,
    Enumeration = 35,
    Timestamptz = 36,
    Timestampltz = 37,
    Datetimetz = 38,
    Datetimeltz = 39,
    Json = 40,
}

impl DbType {
    /// The type for an on-disk id, or `None` for an id the version does not store.
    pub fn from_id(version: VersionLine, id: u32) -> Option<DbType> {
        use DbType::*;
        let t = match id {
            0 => Null,
            1 => Integer,
            2 => Float,
            3 => Double,
            4 => Varchar,
            5 => Object,
            6 => Set,
            7 => Multiset,
            8 => Sequence,
            9 => Elo,
            10 => Time,
            11 => Timestamp,
            12 => Date,
            13 => Monetary,
            14 => Variable,
            15 => Sub,
            18 => Short,
            19 => Vobj,
            20 => Oid,
            22 => Numeric,
            23 => Bit,
            24 => Varbit,
            25 => Char,
            26 if matches!(version, VersionLine::V10_2 | VersionLine::V11_4) => Nchar,
            27 if matches!(version, VersionLine::V10_2 | VersionLine::V11_4) => Varnchar,
            28 => Resultset,
            29 => Midxkey,
            30 => Table,
            31 => Bigint,
            32 => Datetime,
            33 => Blob,
            34 => Clob,
            35 => Enumeration,
            36 => Timestamptz,
            37 => Timestampltz,
            38 => Datetimetz,
            39 => Datetimeltz,
            40 => Json,
            _ => return None,
        };
        Some(t)
    }

    pub fn id(self) -> u8 {
        self as u8
    }

    /// Engine `TP_IS_SET_TYPE` (`SET`, `MULTISET`, `SEQUENCE`; `VOBJ` is packed the same way).
    pub fn is_set(self) -> bool {
        matches!(
            self,
            DbType::Set | DbType::Multiset | DbType::Sequence | DbType::Vobj
        )
    }

    /// Character string types (engine `TP_IS_CHAR_TYPE`).
    pub fn is_char_string(self) -> bool {
        matches!(
            self,
            DbType::Char | DbType::Varchar | DbType::Nchar | DbType::Varnchar
        )
    }

    /// Fixed disk size that does not depend on the domain's precision, or `None`.
    /// Engine `tp_*.disksize` with `variable_p == 0`.
    pub fn constant_disk_size(self, version: VersionLine) -> Option<usize> {
        use DbType::*;
        Some(match self {
            Null => 0,
            Integer | Float | Time | Timestamp | Timestampltz | Date => 4,
            Short | Enumeration => 2,
            Bigint | Double | Datetime | Datetimeltz | Timestamptz | Object | Oid => 8,
            Datetimetz | Monetary => 12,
            // 11.4 `OR_NUMERIC_SIZE` = `DB_NUMERIC_BUF_SIZE` 16 B; develop NUMERIC is variable (CBRD-26006 #6486).
            Numeric if matches!(version, VersionLine::V10_2 | VersionLine::V11_4) => 16,
            _ => return None,
        })
    }
}
