//! The `_db_attribute.default_value` string of one attribute: its default value or default expression as text.
//!
//! Input: a develop attribute (packed current default value and packed property sequence). Output: the string, or
//! `None` for no default.
//! Source: engine develop @682b080b2 `catalog_class.c:catcls_get_or_value_from_attribute` (the `default_expr`
//! property wins over the value: `<expr>` or `TO_CHAR(<expr>[, '<format>'])`; `update_default` appends
//! `ON UPDATE <expr>`; else the value through `valcnv_convert_value_to_string`), `db_macro.c`
//! `db_default_expression_string`, `valcnv_convert_data_to_string` (integers in decimal, float/double `%.17g`, NUMERIC `numeric_db_value_print`, DATE/TIME/DATETIME `db_date.c` `db_*_to_string`, CHAR/VARCHAR bare with `'`
//! doubled); R4 § 7 R4 (2048-byte cut).

use fastmig_ondisk::VersionLine;
use fastmig_ondisk::class_object::Attribute;
use fastmig_ondisk::value::{
    DecodeError, DecodeResult, Numeric, OrReader, SetValue, Value, decode_set, decode_tagged,
};

const LINE: VersionLine = VersionLine::Develop;
/// Engine `DB_MAX_DEFAULT_EXPR_LENGTH`.
const MAX_DEFAULT_LENGTH: usize = 2048;
/// Engine `DB_MAX_IDENTIFIER_LENGTH`.
const MAX_IDENTIFIER_LENGTH: usize = 255;

fn err(what: String) -> DecodeError {
    DecodeError { at: 0, what }
}

/// Engine `db_default_expression_string` (`DB_DEFAULT_EXPR_TYPE` → text).
fn expr_string(t: i32) -> DecodeResult<&'static str> {
    Ok(match t {
        1 => "SYS_DATE",
        2 => "SYS_DATETIME",
        3 => "SYS_TIMESTAMP",
        4 => "UNIX_TIMESTAMP()",
        5 => "USER()",
        6 => "CURRENT_USER",
        7 => "CURRENT_DATETIME",
        8 => "CURRENT_TIMESTAMP",
        9 => "CURRENT_TIME",
        10 => "CURRENT_DATE",
        11 => "SYS_TIME",
        13 => "SYS_GUID()",
        14 => "UUID(4)",
        15 => "UUID(7)",
        _ => return Err(err(format!("default expression type {t}"))),
    })
}

fn property<'a>(props: &'a SetValue, name: &[u8]) -> Option<&'a Value> {
    props
        .elements
        .chunks(2)
        .find_map(|p| match (&p[0].value, p.get(1)) {
            (Value::Varchar(n) | Value::Char(n), Some(v)) if n == name => Some(&v.value),
            _ => None,
        })
}

/// `default_expr`: an integer type, or `{T_TO_CHAR, type, format}`.
fn default_expr(v: &Value) -> DecodeResult<String> {
    match v {
        Value::Integer(t) => Ok(expr_string(*t)?.to_string()),
        Value::Set(s) if s.elements.len() == 3 => {
            let Value::Integer(t) = s.elements[1].value else {
                return Err(err(format!("default_expr type {:?}", s.elements[1].value)));
            };
            let e = expr_string(t)?;
            Ok(match &s.elements[2].value {
                Value::Varchar(f) | Value::Char(f) => {
                    format!("TO_CHAR({e}, '{}')", String::from_utf8_lossy(f))
                }
                _ => format!("TO_CHAR({e})"),
            })
        }
        v => Err(err(format!("default_expr {v:?}"))),
    }
}

/// Engine `valcnv_convert_data_to_string` for the value types a default can take here.
fn value_string(v: &Value) -> DecodeResult<Option<String>> {
    Ok(Some(match v {
        Value::Null => return Ok(None),
        Value::Integer(i) => i.to_string(),
        Value::Short(i) => i.to_string(),
        Value::Bigint(i) => i.to_string(),
        Value::Char(b) | Value::Varchar(b) => String::from_utf8_lossy(b).replace('\'', "''"),
        Value::Double(d) => g17(if d.is_infinite() {
            f64::MAX.copysign(*d)
        } else {
            *d
        }),
        Value::Float(f) => g17(if f.is_infinite() {
            f32::MAX.copysign(*f)
        } else {
            *f
        } as f64),
        Value::Numeric(n) => numeric_string(n),
        Value::Date(d) => date_string(*d),
        Value::Time(t) => {
            let (h, m, s) = (t / 3600, t / 60 % 60, t % 60);
            format!("{:02}:{m:02}:{s:02} {}", hour12(h), ampm(h))
        }
        Value::Datetime { date, time } => {
            let (h, m, s, ms) = (
                time / 3_600_000,
                time / 60_000 % 60,
                time / 1000 % 60,
                time % 1000,
            );
            format!(
                "{:02}:{m:02}:{s:02}.{ms:03} {} {}",
                hour12(h),
                ampm(h),
                date_string(*date)
            )
        }
        // shortcut: other default types (timestamps and time-zone types, bits, enums, monetary, sets) refuse; the T1 databases have
        // none; port the remaining valcnv cases when a source database brings one
        v => {
            return Err(err(format!(
                "default value {v:?} has no string conversion yet"
            )));
        }
    }))
}

pub fn default_value(a: &Attribute) -> DecodeResult<Option<String>> {
    let value = if a.default_value.is_empty() {
        Value::Null
    } else {
        let p = &a.default_value;
        decode_tagged(LINE, &mut OrReader::new(p), Some(p.len()))?.1
    };
    let props = if a.properties.is_empty() {
        None
    } else {
        Some(decode_set(LINE, &mut OrReader::new(&a.properties), None)?)
    };
    let mut s = match props.as_ref().and_then(|p| property(p, b"default_expr")) {
        Some(e) => Some(default_expr(e)?),
        // the engine cuts a value at DB_MAX_IDENTIFIER_LENGTH when the attribute has properties
        None if props.is_some() => {
            value_string(&value)?.map(|s| truncate(s, MAX_IDENTIFIER_LENGTH))
        }
        None => value_string(&value)?,
    };
    if let Some(Value::Integer(t)) = props.as_ref().and_then(|p| property(p, b"update_default")) {
        let u = expr_string(*t)?;
        s = Some(match s {
            Some(d) => format!("{d} ON UPDATE {u}"),
            None => format!("ON UPDATE {u}"),
        });
    }
    Ok(s.map(|s| truncate(s, MAX_DEFAULT_LENGTH)))
}

/// C `printf("%.17g")`: 17 significant digits, exponent form when the exponent is below -4 or at least 17,
/// trailing zeros dropped.
fn g17(v: f64) -> String {
    if v == 0.0 {
        return if v.is_sign_negative() { "-0" } else { "0" }.into();
    }
    let sci = format!("{v:.16e}");
    let (mant, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let trim = |s: String| {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s
        }
    };
    if !(-4..17).contains(&exp) {
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{}e{sign}{:02}", trim(mant.to_string()), exp.abs())
    } else {
        trim(format!("{v:.*}", (16 - exp).max(0) as usize))
    }
}

/// Engine `numeric_db_value_print` with `oracle_compat_number_behavior` off (its default): the unscaled digits
/// with a point `scale` digits from the right, one digit kept before the point, zeros appended for a negative
/// scale.
fn numeric_string(n: &Numeric) -> String {
    let digits = n
        .magnitude
        .iter()
        .fold(0u128, |acc, b| (acc << 8) | *b as u128)
        .to_string();
    let scale = n.scale.max(0) as usize;
    let padded = format!("{digits:0>width$}", width = scale + 1);
    let (int, frac) = padded.split_at(padded.len() - scale);
    let mut s = String::new();
    if n.negative {
        s.push('-');
    }
    s.push_str(int);
    if scale > 0 {
        s.push('.');
        s.push_str(frac);
    }
    if n.scale < 0 && digits != "0" {
        s.push_str(&"0".repeat(n.scale.unsigned_abs() as usize));
    }
    s
}

fn hour12(h: u32) -> u32 {
    if h.is_multiple_of(12) { 12 } else { h % 12 }
}

fn ampm(h: u32) -> &'static str {
    if h >= 12 { "PM" } else { "AM" }
}

/// Engine `db_date_to_string`: `MM/DD/YYYY` of a Julian day number.
fn date_string(jul: u32) -> String {
    let (m, d, y) = julian_decode(jul as i32);
    format!("{m:02}/{d:02}/{y:04}")
}

/// Engine `db_date.c:julian_decode` (month, day, year), with its float casts.
fn julian_decode(jul: i32) -> (i32, i32, i32) {
    /// Engine `IGREG2` (10/15/1582) and `IGREG_SPECIAL`.
    const IGREG2: i32 = 2_299_161;
    let ja = if jul >= IGREG2 {
        let alpha = ((((jul - 1_867_216) as f32) as f64 - 0.25) / 36524.25) as i32;
        jul + 1 + alpha - (0.25 * alpha as f64) as i32
    } else {
        jul
    };
    if ja == 0 {
        return (0, 0, 0);
    }
    let jb = ja + 1524;
    let jc = (6680.0 + (((jb - 2_439_870) as f32) as f64 - 122.1) / 365.25) as i32;
    let jd = (365.0 * jc as f64 + 0.25 * jc as f64) as i32;
    let je = ((jb - jd) as f64 / 30.6001) as i32;
    let day = jb - jd - (30.6001 * je as f64) as i32;
    let month = if je - 1 > 12 { je - 13 } else { je - 1 };
    let mut year = jc - 4715;
    if month > 2 {
        year -= 1;
    }
    if year <= 0 {
        year -= 1;
    }
    (month, day, year)
}

/// Engine `db_string_truncate`: at most `n` characters.
fn truncate(s: String, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((at, _)) => s[..at].to_string(),
        None => s,
    }
}
