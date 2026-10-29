//! A decoded value, one variant per disk encoding the engine has (engine `DB_VALUE` as it reaches disk).
//!
//! Input: produced by [`crate::value::codec`] and the set codec. Output: [`Value`], compared field by field to
//! check a re-encoding, and handed back to the encoders.
//! Source: engine `src/compat/dbtype_def.h` (`DB_DATA`, `DB_DATETIME`, `DB_TIMESTAMPTZ`, `DB_MONETARY`, `DB_ELO`),
//! `src/object/object_primitive.c` per-type readers (release/11.4 @46212adcd, develop @5b1867174).

use crate::Oid;
use crate::value::db_type::DbType;
use crate::value::domain::Domain;
use crate::value::elo::Elo;
use crate::value::numeric::Numeric;

/// One value. Integers keep their raw on-disk meaning: dates are Julian day numbers, times seconds, timestamps
/// UNIX seconds, `time` of a DATETIME milliseconds, `tz_id` the engine's packed `TZ_ID`.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Integer(i32),
    Short(i16),
    Bigint(i64),
    Float(f32),
    Double(f64),
    Monetary {
        currency: i32,
        amount: f64,
    },
    Date(u32),
    Time(u32),
    Timestamp(u32),
    Timestampltz(u32),
    Timestamptz {
        timestamp: u32,
        tz_id: u32,
    },
    Datetime {
        date: u32,
        time: u32,
    },
    Datetimeltz {
        date: u32,
        time: u32,
    },
    Datetimetz {
        date: u32,
        time: u32,
        tz_id: u32,
    },
    Numeric(Numeric),
    /// CHAR: for a declared CHAR(n) the bytes of its n characters (space padded), as both versions' writers keep it.
    Char(Vec<u8>),
    Varchar(Vec<u8>),
    /// 11.4 NCHAR(n); written to develop as CHAR (workspace#482).
    Nchar(Vec<u8>),
    /// 11.4 VARNCHAR; written to develop as VARCHAR with the same bytes (workspace#482).
    Varnchar(Vec<u8>),
    /// BIT(n) / BIT VARYING: length in bits and the bytes holding them.
    Bit {
        bits: u32,
        bytes: Vec<u8>,
    },
    Varbit {
        bits: u32,
        bytes: Vec<u8>,
    },
    Object(Oid),
    Oid(Oid),
    /// ENUM: 1-based label index, 0 for the empty-string value.
    Enumeration(u16),
    Set(SetValue),
    Elo(Elo),
    /// JSON in the engine's binary serialization (`db_json_serialize`) with string padding zeroed; same in both
    /// versions ([`crate::value::json`]).
    Json(Vec<u8>),
}

/// A SET, MULTISET or SEQUENCE.
#[derive(Clone, Debug, PartialEq)]
pub struct SetValue {
    pub ty: DbType,
    /// The set's domain when the packed set carries it in its header (engine `OR_SET_DOMAIN_BIT`).
    pub domain: Option<Domain>,
    pub elements: Vec<SetElement>,
}

/// One set element and, when the set tags its elements, the element's own domain tag.
#[derive(Clone, Debug, PartialEq)]
pub struct SetElement {
    pub tag: Option<Domain>,
    pub value: Value,
}

impl Value {
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }
}
