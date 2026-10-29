//! A value's domain: type plus the parameters that decide its disk bytes (precision, scale, codeset, collation,
//! element domains of a collection, enum labels).
//!
//! Input: built by the packed-domain decoder ([`crate::value::packed_domain`]) or by the class-object decoder
//! (catalog crate). Output: [`Domain`] and its fixed disk size per version.
//! Source: engine `src/object/object_domain.h` (`TP_DOMAIN`), `object_domain.c` `tp_domain_disk_size`
//! (release/11.4 @46212adcd :11082, develop @5b1867174 :6826) and the built-in domains `tp_*_domain`; R2 § 2.

use crate::Oid;
use crate::VersionLine;
use crate::value::db_type::DbType;
use crate::value::string::{codeset, str_size};

/// Engine `TP_FLOATING_PRECISION_VALUE`: "no declared precision" (CHAR/BIT tags, NUMERIC with floating precision).
pub const FLOATING_PRECISION: i32 = -1;
/// Engine `DB_MAX_VARCHAR_PRECISION` = `DB_MAX_STRING_LENGTH` (0x3fffffff), both versions.
pub const MAX_VARCHAR_PRECISION: i32 = 0x3fff_ffff;
/// Engine `DB_MAX_VARBIT_PRECISION` = `DB_MAX_BIT_LENGTH` (0x3fffffff), both versions.
pub const MAX_VARBIT_PRECISION: i32 = 0x3fff_ffff;
/// Engine 11.4 `DB_MAX_VARNCHAR_PRECISION` = `DB_MAX_STRING_LENGTH / 2`.
pub const MAX_VARNCHAR_PRECISION_11_4: i32 = 0x3fff_ffff / 2;
/// Engine `DB_DEFAULT_NUMERIC_PRECISION` / `DB_DEFAULT_NUMERIC_SCALE` (the built-in NUMERIC domain).
pub const DEFAULT_NUMERIC_PRECISION_11_4: i32 = 15;
pub const DEFAULT_NUMERIC_PRECISION_DEVELOP: i32 = 40;

/// Engine `TP_DOMAIN_COLL_ACTION`, stored in the high bits of a packed domain's collation word.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum CollationFlag {
    #[default]
    Normal,
    Enforce,
    Leave,
}

/// Engine `TP_DOMAIN`, the parts that reach disk. `next` chains become `Vec<Domain>` (see `set_domain`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Domain {
    pub ty: DbType,
    /// Characters for CHAR/VARCHAR, bits for BIT/VARBIT, digits for NUMERIC, key count for MIDXKEY;
    /// [`FLOATING_PRECISION`] when undeclared.
    pub precision: i32,
    pub scale: i32,
    /// Engine `INTL_CODESET` ([`crate::value::string::codeset`]).
    pub codeset: u8,
    pub collation_id: i32,
    pub collation_flag: CollationFlag,
    /// DESC key column (index key domains only).
    pub is_desc: bool,
    /// OBJECT: the referenced class, when the packing included it.
    pub class_oid: Option<Oid>,
    /// SET/MULTISET/SEQUENCE/MIDXKEY: the element domains (engine `setdomain` and its `next` chain).
    pub set_domain: Vec<Domain>,
    /// ENUM: labels in order; value `k` (1-based) names label `k - 1`.
    pub enum_labels: Vec<Vec<u8>>,
    /// JSON: the schema text, when the column has one.
    pub json_schema: Option<Vec<u8>>,
    /// Packed as a built-in domain reference (a single word naming only the type).
    pub builtin: bool,
}

impl Domain {
    /// A domain with only the type set, codeset and collation as the engine's built-in domain of that type.
    pub fn of_type(ty: DbType) -> Domain {
        let (cs, coll) = match ty {
            DbType::Bit | DbType::Varbit => (codeset::RAW_BITS, 0),
            // `tp_Json_domain`: UTF-8, `LANG_COLL_UTF8_BINARY` (2)
            DbType::Json => (codeset::UTF8, 2),
            DbType::Char
            | DbType::Varchar
            | DbType::Nchar
            | DbType::Varnchar
            | DbType::Enumeration => (codeset::ISO88591, 0),
            _ => (0, 0),
        };
        Domain {
            ty,
            precision: 0,
            scale: 0,
            codeset: cs,
            collation_id: coll,
            collation_flag: CollationFlag::Normal,
            is_desc: false,
            class_oid: None,
            set_domain: Vec::new(),
            enum_labels: Vec::new(),
            json_schema: None,
            builtin: false,
        }
    }

    /// Engine `tp_domain_resolve_default (type)`: the built-in domain a packed built-in reference names.
    pub fn builtin(version: VersionLine, ty: DbType) -> Domain {
        let mut d = Domain::of_type(ty);
        d.builtin = true;
        d.precision = match ty {
            DbType::Varchar => MAX_VARCHAR_PRECISION,
            DbType::Varbit => MAX_VARBIT_PRECISION,
            DbType::Varnchar => MAX_VARNCHAR_PRECISION_11_4,
            DbType::Char | DbType::Nchar | DbType::Bit => FLOATING_PRECISION,
            DbType::Numeric => match version {
                VersionLine::V10_2 | VersionLine::V11_4 => DEFAULT_NUMERIC_PRECISION_11_4,
                VersionLine::Develop => DEFAULT_NUMERIC_PRECISION_DEVELOP,
            },
            _ => 0,
        };
        d
    }

    /// Engine `tp_domain_disk_size`: bytes in the record's fixed area, or `None` for a variable attribute.
    pub fn disk_size(&self, version: VersionLine) -> Option<usize> {
        if let Some(n) = self.ty.constant_disk_size(version) {
            return Some(n);
        }
        let fixed_string = matches!(
            (version, self.ty),
            (_, DbType::Bit)
                | (
                    VersionLine::V10_2 | VersionLine::V11_4,
                    DbType::Char | DbType::Nchar
                )
        );
        if fixed_string && self.precision != FLOATING_PRECISION {
            Some(str_size(self.precision, self.codeset))
        } else {
            None
        }
    }
}
