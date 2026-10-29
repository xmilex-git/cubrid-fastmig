//! Per-type value codec: decode or encode one value given its domain, or a tagged value that carries its domain.
//!
//! Input: OR bytes at a cursor, the version, and the domain (or none for a tagged value), plus the bytes the value
//! may use when the container knows it. Output: [`Value`] (and the tag), or the value's bytes in a chosen version.
//! Source: engine `src/object/object_primitive.c` `mr_data_readval_*`/`mr_data_writeval_*` and
//! `src/object/object_representation.c` `or_get_value`/`or_put_value` (develop @5b1867174 :4819-5036; same in
//! release/11.4 @46212adcd); per-type rules R2 § 2.

use crate::Oid;
use crate::VersionLine;
use crate::value::db_type::DbType;
use crate::value::db_value::Value;
use crate::value::domain::{Domain, FLOATING_PRECISION};
use crate::value::elo::{read_elo, write_elo};
use crate::value::json::read_json;
use crate::value::numeric::{
    read_numeric_11_4, read_numeric_develop, write_numeric_11_4, write_numeric_develop,
};
use crate::value::or_buf::{DecodeResult, OrReader, OrWriter};
use crate::value::packed_domain::{ValueTag, decode_value_tag, encode_packed_domain};
use crate::value::set::{decode_set, encode_set};
use crate::value::string::{
    StringCompression, read_fixed_char_11_4, read_string, str_size, write_fixed_char_11_4,
    write_string,
};

fn read_oid(r: &mut OrReader) -> DecodeResult<Oid> {
    let pageid = r.i32()?;
    let slotid = r.i16()?;
    let volid = r.i16()?;
    Ok(Oid::new(volid, pageid, slotid))
}

fn write_oid(w: &mut OrWriter, o: &Oid) {
    w.i32(o.pageid);
    w.i16(o.slotid);
    w.i16(o.volid);
}

fn bits_to_bytes(bits: u32) -> usize {
    (bits as usize).div_ceil(8)
}

/// Decodes the data of one non-NULL value of domain `d` (engine `data_readval`); every type's data carries its own
/// length, so the cursor ends right after it (the container adds any padding).
pub fn decode_data(version: VersionLine, r: &mut OrReader, d: &Domain) -> DecodeResult<Value> {
    use DbType as T;
    Ok(match d.ty {
        T::Null => Value::Null,
        T::Integer => Value::Integer(r.i32()?),
        T::Short => Value::Short(r.i16()?),
        T::Bigint => Value::Bigint(r.i64()?),
        T::Float => Value::Float(f32::from_bits(r.u32()?)),
        T::Double => Value::Double(f64::from_bits(r.i64()? as u64)),
        T::Monetary => Value::Monetary {
            currency: r.i32()?,
            amount: f64::from_bits(r.i64()? as u64),
        },
        T::Date => Value::Date(r.u32()?),
        T::Time => Value::Time(r.u32()?),
        T::Timestamp => Value::Timestamp(r.u32()?),
        T::Timestampltz => Value::Timestampltz(r.u32()?),
        T::Timestamptz => Value::Timestamptz {
            timestamp: r.u32()?,
            tz_id: r.u32()?,
        },
        T::Datetime => Value::Datetime {
            date: r.u32()?,
            time: r.u32()?,
        },
        T::Datetimeltz => Value::Datetimeltz {
            date: r.u32()?,
            time: r.u32()?,
        },
        T::Datetimetz => Value::Datetimetz {
            date: r.u32()?,
            time: r.u32()?,
            tz_id: r.u32()?,
        },
        T::Object => Value::Object(read_oid(r)?),
        T::Oid => Value::Oid(read_oid(r)?),
        T::Enumeration => Value::Enumeration(r.u16()?),
        T::Numeric => Value::Numeric(match version {
            VersionLine::V10_2 | VersionLine::V11_4 => read_numeric_11_4(r, d.precision, d.scale)?,
            VersionLine::Develop => read_numeric_develop(r, d.precision)?,
        }),
        T::Char | T::Nchar => {
            let s = match version {
                VersionLine::Develop => read_string(r, StringCompression::of(version))?,
                // 11.4 CHAR without a precision (tags only): BE length then the bytes
                VersionLine::V10_2 | VersionLine::V11_4 if d.precision == FLOATING_PRECISION => {
                    let n = r.i32()?;
                    r.bytes(n.max(0) as usize)?.to_vec()
                }
                VersionLine::V10_2 | VersionLine::V11_4 => {
                    read_fixed_char_11_4(r, d.precision, d.codeset)?
                }
            };
            if d.ty == T::Nchar {
                Value::Nchar(s)
            } else {
                Value::Char(s)
            }
        }
        T::Varchar => Value::Varchar(read_string(r, StringCompression::of(version))?),
        T::Varnchar => Value::Varnchar(read_string(r, StringCompression::of(version))?),
        T::Bit => {
            if d.precision == FLOATING_PRECISION {
                let bits = r.i32()?.max(0) as u32;
                Value::Bit {
                    bits,
                    bytes: r.bytes(bits_to_bytes(bits))?.to_vec(),
                }
            } else {
                Value::Bit {
                    bits: d.precision as u32,
                    bytes: r.bytes(str_size(d.precision, d.codeset))?.to_vec(),
                }
            }
        }
        T::Varbit => {
            // `or_get_varbit_length`: 1-byte bit count, or 0xFF then a BE int
            let b = r.u8()?;
            let bits = if b < 0xFF {
                b as u32
            } else {
                r.i32()?.max(0) as u32
            };
            let bytes = r.bytes(bits_to_bytes(bits))?.to_vec();
            r.align4()?;
            Value::Varbit { bits, bytes }
        }
        T::Set | T::Multiset | T::Sequence | T::Vobj => {
            Value::Set(decode_set(version, r, Some(d))?)
        }
        T::Blob | T::Clob | T::Elo => Value::Elo(read_elo(r, d.ty)?),
        T::Json => Value::Json(read_json(r)?),
        t => return Err(r.error(format!("{t:?} has no value codec"))),
    })
}

/// Encodes the data of one non-NULL value in `version` under domain `d` (engine `data_writeval`). The value's
/// variant may be an equivalent one of another version (NCHAR bytes written as CHAR); a value that does not match
/// the domain's type panics, as that is a caller error.
pub fn encode_data(version: VersionLine, w: &mut OrWriter, d: &Domain, v: &Value) {
    use DbType as T;
    match (d.ty, v) {
        (T::Integer, Value::Integer(x)) => w.i32(*x),
        (T::Short, Value::Short(x)) => w.i16(*x),
        (T::Bigint, Value::Bigint(x)) => w.i64(*x),
        (T::Float, Value::Float(x)) => w.u32(x.to_bits()),
        (T::Double, Value::Double(x)) => w.i64(x.to_bits() as i64),
        (T::Monetary, Value::Monetary { currency, amount }) => {
            w.i32(*currency);
            w.i64(amount.to_bits() as i64);
        }
        (T::Date, Value::Date(x))
        | (T::Time, Value::Time(x))
        | (T::Timestamp, Value::Timestamp(x))
        | (T::Timestampltz, Value::Timestampltz(x)) => w.u32(*x),
        (T::Timestamptz, Value::Timestamptz { timestamp, tz_id }) => {
            w.u32(*timestamp);
            w.u32(*tz_id);
        }
        (T::Datetime, Value::Datetime { date, time })
        | (T::Datetimeltz, Value::Datetimeltz { date, time }) => {
            w.u32(*date);
            w.u32(*time);
        }
        (T::Datetimetz, Value::Datetimetz { date, time, tz_id }) => {
            w.u32(*date);
            w.u32(*time);
            w.u32(*tz_id);
        }
        (T::Object, Value::Object(o)) | (T::Oid, Value::Oid(o)) => write_oid(w, o),
        (T::Enumeration, Value::Enumeration(x)) => w.u16(*x),
        (T::Numeric, Value::Numeric(n)) => match version {
            VersionLine::V10_2 | VersionLine::V11_4 => write_numeric_11_4(w, n),
            VersionLine::Develop => write_numeric_develop(w, n),
        },
        (T::Char | T::Nchar, Value::Char(s) | Value::Nchar(s)) => match version {
            VersionLine::Develop => write_string(w, s),
            VersionLine::V10_2 | VersionLine::V11_4 if d.precision == FLOATING_PRECISION => {
                w.i32(s.len() as i32);
                w.bytes(s);
            }
            VersionLine::V10_2 | VersionLine::V11_4 => {
                write_fixed_char_11_4(w, s, d.precision, d.codeset)
            }
        },
        (T::Varchar | T::Varnchar, Value::Varchar(s) | Value::Varnchar(s)) => write_string(w, s),
        (T::Bit, Value::Bit { bits, bytes }) => {
            if d.precision == FLOATING_PRECISION {
                w.i32(*bits as i32);
                w.bytes(bytes);
            } else {
                let size = str_size(d.precision, d.codeset);
                w.bytes(&bytes[..bytes.len().min(size)]);
                w.zeros(size.saturating_sub(bytes.len()));
            }
        }
        (T::Varbit, Value::Varbit { bits, bytes }) => {
            if *bits < 0xFF {
                w.u8(*bits as u8);
            } else {
                w.u8(0xFF);
                w.i32(*bits as i32);
            }
            w.bytes(bytes);
            w.align4();
        }
        (T::Set | T::Multiset | T::Sequence | T::Vobj, Value::Set(s)) => {
            encode_set(version, w, Some(d), s)
        }
        (T::Blob | T::Clob | T::Elo, Value::Elo(e)) => write_elo(w, e),
        (T::Json, Value::Json(b)) => w.bytes(b),
        (t, v) => panic!("value {v:?} does not match domain type {t:?}"),
    }
}

/// Decodes one tagged value (engine `or_get_value` with no domain): a packed domain tag, then the data unless the
/// tag is a NULL tag, then padding up to `expected` bytes or the next 4-byte boundary. Returns the tag and value.
pub fn decode_tagged(
    version: VersionLine,
    r: &mut OrReader,
    expected: Option<usize>,
) -> DecodeResult<(Domain, Value)> {
    let start = r.pos();
    let ValueTag { mut chain, is_null } = decode_value_tag(version, r)?;
    if chain.is_empty() {
        return Err(r.error("empty domain tag"));
    }
    let tag = chain.swap_remove(0);
    let value = if is_null {
        Value::Null
    } else {
        decode_data(version, r, &tag)?
    };
    finish_padding(r, start, expected)?;
    Ok((tag, value))
}

/// Decodes one value at the cursor (engine `or_get_value`): `domain` given → untagged data; `None` → a tagged value
/// ([`decode_tagged`], tag dropped). `expected == Some(0)` is a collapsed NULL.
pub fn decode_value(
    version: VersionLine,
    r: &mut OrReader,
    domain: Option<&Domain>,
    expected: Option<usize>,
) -> DecodeResult<Value> {
    if expected == Some(0) {
        return Ok(Value::Null);
    }
    match domain {
        None => Ok(decode_tagged(version, r, expected)?.1),
        Some(d) => {
            let start = r.pos();
            let v = decode_data(version, r, d)?;
            finish_padding(r, start, expected)?;
            Ok(v)
        }
    }
}

fn finish_padding(r: &mut OrReader, start: usize, expected: Option<usize>) -> DecodeResult<()> {
    let used = r.pos() - start;
    match expected {
        Some(n) if n > used => r.skip(n - used),
        Some(_) => Ok(()),
        None => r.align4(),
    }
}

/// Encodes one tagged value (engine `or_put_value` with `include_domain`): the tag (NULL bit set for
/// [`Value::Null`]), the data, then padding to 4 bytes from the value's start.
pub fn encode_tagged(version: VersionLine, w: &mut OrWriter, tag: &Domain, v: &Value) {
    let start = w.len();
    encode_packed_domain(version, w, std::slice::from_ref(tag), false, v.is_null());
    if !v.is_null() {
        encode_data(version, w, tag, v);
    }
    let pad = (4 - ((w.len() - start) & 3)) & 3;
    w.zeros(pad);
}
