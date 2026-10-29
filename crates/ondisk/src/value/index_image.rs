//! One value as a B-tree key stores it (engine `index_writeval`): its length, its value, its bytes, and the order of
//! two such images under the column's domain and collation.
//!
//! Input: the start of one key element and its column domain. Output: element length, [`Value`], the encoded
//! element, or an [`Ordering`].
//! Formats (both versions unless noted): fixed-size values are host order (little endian) — INTEGER 4, SHORT 2,
//! BIGINT 8, FLOAT 4, DOUBLE 8, DATE/TIME/TIMESTAMP(LTZ) 4, TIMESTAMPTZ 4+4 (tz), DATETIME(LTZ) date 4 + time 4,
//! DATETIMETZ +4 (tz), MONETARY type 4 + amount 8, OBJECT/OID pageid 4 + slotid 2 + volid 2, ENUM 2.
//! VARCHAR (and develop CHAR): `[len:u8 < 255][bytes]` or `[0xFF][compressed:BE i32][len:BE i32][data]`, no NUL,
//! no padding. 11.4 CHAR/NCHAR(n): `STR_SIZE` bytes space padded; without precision a LE i32 length then bytes.
//! NUMERIC: the data encoding (11.4 16 B, develop header form). BIT(n): `STR_SIZE` bytes; without precision LE i32
//! bit count then bytes. VARBIT: `[bits:u8 < 255]` or `[0xFF][bits:BE i32]`, bytes, no padding.
//! Order: numbers by value, DATETIME by (date, time), TZ types by their UTC part, OID by (volid, pageid, slotid),
//! strings by the collation's `fastcmp` (spaces weigh 0; CHAR ignores trailing spaces, VARCHAR does not —
//! `ignore_trailing_space` at its default `no`), BIT/VARBIT bytewise then by length.
//! Source: develop @5b1867174 `src/object/object_primitive.c` `mr_index_writeval_*`, `mr_index_readval_*`,
//! `mr_index_lengthmem_*`, `mr_index_cmpdisk_*` / `mr_cmpdisk_char_internal` (:2450-14343, :11260, :12394);
//! release/11.4 @46212adcd same file (`mr_writeval_char_internal` with `CHAR_ALIGNMENT`);
//! `src/base/language_support.c` `lang_fastcmp_byte`, `lang_fastcmp_binary`, `lang_init_coll_en_cs/_ci`
//! (:5387-5470, :5535, :6497).

use std::borrow::Cow;
use std::cmp::Ordering;

use crate::Oid;
use crate::VersionLine;
use crate::value::db_type::DbType;
use crate::value::db_value::Value;
use crate::value::domain::{Domain, FLOATING_PRECISION};
use crate::value::numeric::{
    Numeric, read_numeric_11_4, read_numeric_develop, write_numeric_11_4, write_numeric_develop,
};
use crate::value::or_buf::{DecodeError, DecodeResult, OrReader, OrWriter};
use crate::value::string::{MIN_LENGTH_FOR_COMPRESSION, str_size};

/// Built-in collations whose order this module implements (engine `LANG_COLL_*` ids, the same in 11.4 and
/// develop, `src/base/language_support.h:107-116`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Collation {
    /// `lang_fastcmp_byte` with the identity weights: iso88591_bin (0), utf8_bin (1), iso88591_en_cs (2),
    /// utf8_en_cs (4).
    ByteCs,
    /// `lang_fastcmp_byte` with 'a'..'z' weighed as 'A'..'Z': iso88591_en_ci (3), utf8_en_ci (5).
    ByteCi,
    /// `lang_strcmp_utf8` with the EN_cs weights: utf8_ko_cs (7) — code points in order, below 256 by the
    /// identity weight, a space weighing 0.
    Utf8Cp,
    /// `lang_fastcmp_binary`: binary (9).
    Binary,
}

/// Engine `intl_utf8_to_cp`: the code point at the start of `s` and its byte length (a malformed byte is taken as
/// itself, one byte long).
fn utf8_cp(s: &[u8]) -> (u32, usize) {
    let b = s[0] as u32;
    let (n, init) = match s[0] {
        0x00..=0x7F => return (b, 1),
        0xC0..=0xDF => (2, b & 0x1F),
        0xE0..=0xEF => (3, b & 0x0F),
        0xF0..=0xF7 => (4, b & 0x07),
        _ => return (b, 1),
    };
    if s.len() < n || s[1..n].iter().any(|c| c & 0xC0 != 0x80) {
        return (b, 1);
    }
    (
        s[1..n]
            .iter()
            .fold(init, |cp, c| (cp << 6) | (*c as u32 & 0x3F)),
        n,
    )
}

impl Collation {
    /// The order of collation `id`, or `None` for the collations this module does not implement (utf8_tr_cs 6,
    /// euckr_bin 8, LDML locales from 32).
    pub fn from_id(id: i32) -> Option<Collation> {
        match id {
            0 | 1 | 2 | 4 => Some(Collation::ByteCs),
            3 | 5 => Some(Collation::ByteCi),
            7 => Some(Collation::Utf8Cp),
            9 => Some(Collation::Binary),
            _ => None,
        }
    }

    fn compare_utf8(a: &[u8], b: &[u8], ti: bool) -> Ordering {
        let w = |cp: u32| if cp == b' ' as u32 { 0 } else { cp };
        let (mut i, mut j) = (0, 0);
        while i < a.len() && j < b.len() {
            let (x, n) = utf8_cp(&a[i..]);
            let (y, m) = utf8_cp(&b[j..]);
            let o = w(x).cmp(&w(y));
            if o != Ordering::Equal {
                return o;
            }
            i += n;
            j += m;
        }
        let (ra, rb) = (&a[i..], &b[j..]);
        if ra.is_empty() && rb.is_empty() {
            Ordering::Equal
        } else if !ti {
            if ra.is_empty() {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        } else if ra.iter().chain(rb).all(|c| *c == b' ') {
            // `lang_str_utf8_trail_zero_weights`: only spaces weigh 0 under the EN_cs weights
            Ordering::Equal
        } else if ra.is_empty() {
            Ordering::Less
        } else {
            Ordering::Greater
        }
    }

    fn weight(self, c: u8) -> u32 {
        match (self, c) {
            (_, b' ') => 0,
            (Collation::ByteCi, b'a'..=b'z') => (c - (b'a' - b'A')) as u32,
            _ => c as u32,
        }
    }

    /// Engine `fastcmp` of the collation; `ti` ignores trailing spaces.
    pub fn compare(self, a: &[u8], b: &[u8], ti: bool) -> Ordering {
        if self == Collation::Utf8Cp {
            return Self::compare_utf8(a, b, ti);
        }
        if self == Collation::Binary {
            let n = a.len().min(b.len());
            return a[..n].cmp(&b[..n]).then_with(|| {
                // a longer string is greater only if a byte past the common part is non-zero
                if a[n..].iter().any(|x| *x > 0) {
                    Ordering::Greater
                } else if b[n..].iter().any(|x| *x > 0) {
                    Ordering::Less
                } else {
                    Ordering::Equal
                }
            });
        }
        let n = a.len().min(b.len());
        for i in 0..n {
            let o = self.weight(a[i]).cmp(&self.weight(b[i]));
            if o != Ordering::Equal {
                return o;
            }
        }
        if a.len() == b.len() {
            return Ordering::Equal;
        }
        if !ti {
            return a.len().cmp(&b.len());
        }
        // with `ti`, the tail of the longer string is weighed against 0; spaces weigh 0
        let (tail, sign) = if a.len() > b.len() {
            (&a[n..], Ordering::Greater)
        } else {
            (&b[n..], Ordering::Less)
        };
        if tail.iter().any(|c| self.weight(*c) > 0) {
            sign
        } else {
            Ordering::Equal
        }
    }
}

fn err(at: usize, what: String) -> DecodeError {
    DecodeError { at, what }
}

fn le_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

/// Develop CHAR and every VARCHAR image: header length and payload length.
fn string_header(b: &[u8]) -> DecodeResult<(usize, usize, usize, usize)> {
    let first = *b
        .first()
        .ok_or_else(|| err(0, "empty string image".into()))?;
    if (first as usize) < MIN_LENGTH_FOR_COMPRESSION {
        return Ok((1, first as usize, 0, first as usize));
    }
    if b.len() < 9 {
        return Err(err(0, "short compressed string header".into()));
    }
    let clen = i32::from_be_bytes(b[1..5].try_into().unwrap()).max(0) as usize;
    let len = i32::from_be_bytes(b[5..9].try_into().unwrap()).max(0) as usize;
    Ok((9, if clen > 0 { clen } else { len }, clen, len))
}

fn string_bytes(b: &[u8]) -> DecodeResult<Cow<'_, [u8]>> {
    let (h, stored, clen, len) = string_header(b)?;
    let data = b
        .get(h..h + stored)
        .ok_or_else(|| err(h, "string image past its buffer".into()))?;
    if clen > 0 {
        let mut out = vec![0u8; len];
        lz4_flex::block::decompress_into(data, &mut out)
            .map_err(|e| err(h, format!("LZ4: {e}")))?;
        Ok(Cow::Owned(out))
    } else {
        Ok(Cow::Borrowed(data))
    }
}

/// Whether the domain's string image is the 11.4 fixed CHAR field.
fn fixed_char_11_4(version: VersionLine, d: &Domain) -> bool {
    version == VersionLine::V11_4 && matches!(d.ty, DbType::Char | DbType::Nchar)
}

/// Bytes the image of one value of domain `d` takes at the start of `b` (engine `mr_index_lengthmem_*`).
pub fn image_len(version: VersionLine, d: &Domain, b: &[u8]) -> DecodeResult<usize> {
    use DbType as T;
    if let Some(n) = d.ty.constant_disk_size(version) {
        return Ok(n);
    }
    Ok(match d.ty {
        T::Numeric => {
            (*b.first()
                .ok_or_else(|| err(0, "empty NUMERIC image".into()))?
                & 0x7F) as usize
        }
        T::Char | T::Nchar if fixed_char_11_4(version, d) => {
            if d.precision == FLOATING_PRECISION {
                4 + le_u32(b, 0) as usize
            } else {
                str_size(d.precision, d.codeset)
            }
        }
        T::Char | T::Varchar | T::Varnchar => {
            let (h, stored, _, _) = string_header(b)?;
            h + stored
        }
        T::Bit => {
            if d.precision == FLOATING_PRECISION {
                4 + (le_u32(b, 0) as usize).div_ceil(8)
            } else {
                str_size(d.precision, d.codeset)
            }
        }
        T::Varbit => {
            let first = *b
                .first()
                .ok_or_else(|| err(0, "empty VARBIT image".into()))?;
            if first < 0xFF {
                1 + (first as usize).div_ceil(8)
            } else {
                5 + (i32::from_be_bytes(b[1..5].try_into().unwrap()).max(0) as usize).div_ceil(8)
            }
        }
        t => return Err(err(0, format!("{t:?} cannot be a key column"))),
    })
}

/// Decodes one key element image (engine `index_readval`).
pub fn decode_image(version: VersionLine, d: &Domain, b: &[u8]) -> DecodeResult<Value> {
    use DbType as T;
    let len = image_len(version, d, b)?;
    let b = b
        .get(..len)
        .ok_or_else(|| err(0, format!("{:?} image needs {len} bytes", d.ty)))?;
    let u32_at = |at: usize| le_u32(b, at);
    Ok(match d.ty {
        T::Integer => Value::Integer(u32_at(0) as i32),
        T::Short => Value::Short(i16::from_le_bytes([b[0], b[1]])),
        T::Bigint => Value::Bigint(i64::from_le_bytes(b.try_into().unwrap())),
        T::Float => Value::Float(f32::from_bits(u32_at(0))),
        T::Double => Value::Double(f64::from_le_bytes(b.try_into().unwrap())),
        T::Monetary => Value::Monetary {
            currency: u32_at(0) as i32,
            amount: f64::from_le_bytes(b[4..12].try_into().unwrap()),
        },
        T::Date => Value::Date(u32_at(0)),
        T::Time => Value::Time(u32_at(0)),
        T::Timestamp => Value::Timestamp(u32_at(0)),
        T::Timestampltz => Value::Timestampltz(u32_at(0)),
        T::Timestamptz => Value::Timestamptz {
            timestamp: u32_at(0),
            tz_id: u32_at(4),
        },
        T::Datetime => Value::Datetime {
            date: u32_at(0),
            time: u32_at(4),
        },
        T::Datetimeltz => Value::Datetimeltz {
            date: u32_at(0),
            time: u32_at(4),
        },
        T::Datetimetz => Value::Datetimetz {
            date: u32_at(0),
            time: u32_at(4),
            tz_id: u32_at(8),
        },
        T::Object | T::Oid => {
            let o = Oid::new(
                i16::from_le_bytes([b[6], b[7]]),
                u32_at(0) as i32,
                i16::from_le_bytes([b[4], b[5]]),
            );
            if d.ty == T::Object {
                Value::Object(o)
            } else {
                Value::Oid(o)
            }
        }
        T::Enumeration => Value::Enumeration(u16::from_le_bytes([b[0], b[1]])),
        T::Numeric => Value::Numeric(match version {
            VersionLine::V11_4 => read_numeric_11_4(&mut OrReader::new(b), d.precision, d.scale)?,
            VersionLine::Develop => read_numeric_develop(&mut OrReader::new(b))?,
        }),
        T::Char | T::Nchar if fixed_char_11_4(version, d) => {
            let s = if d.precision == FLOATING_PRECISION {
                b[4..].to_vec()
            } else {
                crate::value::string::read_fixed_char_11_4(
                    &mut OrReader::new(b),
                    d.precision,
                    d.codeset,
                )?
            };
            if d.ty == T::Nchar {
                Value::Nchar(s)
            } else {
                Value::Char(s)
            }
        }
        T::Char => Value::Char(string_bytes(b)?.into_owned()),
        T::Varchar => Value::Varchar(string_bytes(b)?.into_owned()),
        T::Varnchar => Value::Varnchar(string_bytes(b)?.into_owned()),
        T::Bit => {
            if d.precision == FLOATING_PRECISION {
                Value::Bit {
                    bits: u32_at(0),
                    bytes: b[4..].to_vec(),
                }
            } else {
                Value::Bit {
                    bits: d.precision as u32,
                    bytes: b.to_vec(),
                }
            }
        }
        T::Varbit => {
            let (h, bits) = if b[0] < 0xFF {
                (1, b[0] as u32)
            } else {
                (
                    5,
                    i32::from_be_bytes(b[1..5].try_into().unwrap()).max(0) as u32,
                )
            };
            Value::Varbit {
                bits,
                bytes: b[h..].to_vec(),
            }
        }
        t => return Err(err(0, format!("{t:?} cannot be a key column"))),
    })
}

/// Appends the key element image of `v` (non-NULL) under domain `d` (engine `index_writeval`).
pub fn encode_image(version: VersionLine, d: &Domain, v: &Value, out: &mut Vec<u8>) {
    use DbType as T;
    match (d.ty, v) {
        (T::Integer, Value::Integer(x)) => out.extend_from_slice(&x.to_le_bytes()),
        (T::Short, Value::Short(x)) => out.extend_from_slice(&x.to_le_bytes()),
        (T::Bigint, Value::Bigint(x)) => out.extend_from_slice(&x.to_le_bytes()),
        (T::Float, Value::Float(x)) => out.extend_from_slice(&x.to_le_bytes()),
        (T::Double, Value::Double(x)) => out.extend_from_slice(&x.to_le_bytes()),
        (T::Monetary, Value::Monetary { currency, amount }) => {
            out.extend_from_slice(&currency.to_le_bytes());
            out.extend_from_slice(&amount.to_le_bytes());
        }
        (T::Date, Value::Date(x))
        | (T::Time, Value::Time(x))
        | (T::Timestamp, Value::Timestamp(x))
        | (T::Timestampltz, Value::Timestampltz(x)) => out.extend_from_slice(&x.to_le_bytes()),
        (T::Timestamptz, Value::Timestamptz { timestamp, tz_id }) => {
            out.extend_from_slice(&timestamp.to_le_bytes());
            out.extend_from_slice(&tz_id.to_le_bytes());
        }
        (T::Datetime, Value::Datetime { date, time })
        | (T::Datetimeltz, Value::Datetimeltz { date, time }) => {
            out.extend_from_slice(&date.to_le_bytes());
            out.extend_from_slice(&time.to_le_bytes());
        }
        (T::Datetimetz, Value::Datetimetz { date, time, tz_id }) => {
            out.extend_from_slice(&date.to_le_bytes());
            out.extend_from_slice(&time.to_le_bytes());
            out.extend_from_slice(&tz_id.to_le_bytes());
        }
        (T::Object, Value::Object(o)) | (T::Oid, Value::Oid(o)) | (T::Object, Value::Oid(o)) => {
            out.extend_from_slice(&o.pageid.to_le_bytes());
            out.extend_from_slice(&o.slotid.to_le_bytes());
            out.extend_from_slice(&o.volid.to_le_bytes());
        }
        (T::Enumeration, Value::Enumeration(x)) => out.extend_from_slice(&x.to_le_bytes()),
        (T::Numeric, Value::Numeric(n)) => {
            let mut w = OrWriter::new();
            match version {
                VersionLine::V11_4 => write_numeric_11_4(&mut w, n),
                VersionLine::Develop => write_numeric_develop(&mut w, n),
            }
            out.extend_from_slice(&w.buf);
        }
        (T::Char | T::Nchar, Value::Char(s) | Value::Nchar(s)) if fixed_char_11_4(version, d) => {
            if d.precision == FLOATING_PRECISION {
                out.extend_from_slice(&(s.len() as u32).to_le_bytes());
                out.extend_from_slice(s);
            } else {
                let mut w = OrWriter::new();
                crate::value::string::write_fixed_char_11_4(&mut w, s, d.precision, d.codeset);
                out.extend_from_slice(&w.buf);
            }
        }
        (
            T::Char | T::Varchar | T::Varnchar,
            Value::Char(s) | Value::Nchar(s) | Value::Varchar(s) | Value::Varnchar(s),
        ) => write_image_string(out, s),
        (T::Bit, Value::Bit { bits, bytes }) => {
            if d.precision == FLOATING_PRECISION {
                out.extend_from_slice(&bits.to_le_bytes());
                out.extend_from_slice(bytes);
            } else {
                let size = str_size(d.precision, d.codeset);
                out.extend_from_slice(&bytes[..bytes.len().min(size)]);
                out.resize(out.len() + size.saturating_sub(bytes.len()), 0);
            }
        }
        (T::Varbit, Value::Varbit { bits, bytes }) => {
            if *bits < 0xFF {
                out.push(*bits as u8);
            } else {
                out.push(0xFF);
                out.extend_from_slice(&(*bits as i32).to_be_bytes());
            }
            out.extend_from_slice(bytes);
        }
        (t, v) => panic!("value {v:?} cannot be a key of type {t:?}"),
    }
}

/// VARCHAR image with `CHAR_ALIGNMENT`: the data-string header and bytes, no NUL, no padding; strings from 255 bytes
/// are LZ4-compressed when that saves more than 8 bytes, as the engine's writer does.
fn write_image_string(out: &mut Vec<u8>, s: &[u8]) {
    if s.len() < MIN_LENGTH_FOR_COMPRESSION {
        out.push(s.len() as u8);
        out.extend_from_slice(s);
        return;
    }
    let c = lz4_flex::block::compress(s);
    out.push(0xFF);
    if c.len() < s.len() - 8 {
        out.extend_from_slice(&(c.len() as i32).to_be_bytes());
        out.extend_from_slice(&(s.len() as i32).to_be_bytes());
        out.extend_from_slice(&c);
    } else {
        out.extend_from_slice(&0i32.to_be_bytes());
        out.extend_from_slice(&(s.len() as i32).to_be_bytes());
        out.extend_from_slice(s);
    }
}

fn magnitude_cmp(a: &Numeric, b: &Numeric) -> Ordering {
    let za = a.magnitude.iter().all(|x| *x == 0);
    let zb = b.magnitude.iter().all(|x| *x == 0);
    let sa = if za {
        0
    } else if a.negative {
        -1
    } else {
        1
    };
    let sb = if zb {
        0
    } else if b.negative {
        -1
    } else {
        1
    };
    sa.cmp(&sb).then_with(|| {
        let m = a.magnitude.cmp(&b.magnitude);
        if sa < 0 { m.reverse() } else { m }
    })
}

/// Order of two non-NULL images of one column (ascending; the caller flips DESC). `coll` is the column's collation
/// for string types.
pub fn compare_images(
    version: VersionLine,
    d: &Domain,
    coll: Option<Collation>,
    a: &[u8],
    b: &[u8],
) -> Ordering {
    use DbType as T;
    let i32_at = |x: &[u8], at: usize| i32::from_le_bytes(x[at..at + 4].try_into().unwrap());
    match d.ty {
        T::Integer => i32_at(a, 0).cmp(&i32_at(b, 0)),
        T::Short => i16::from_le_bytes([a[0], a[1]]).cmp(&i16::from_le_bytes([b[0], b[1]])),
        T::Bigint => i64::from_le_bytes(a[..8].try_into().unwrap())
            .cmp(&i64::from_le_bytes(b[..8].try_into().unwrap())),
        T::Float => f32::from_le_bytes(a[..4].try_into().unwrap())
            .partial_cmp(&f32::from_le_bytes(b[..4].try_into().unwrap()))
            .unwrap_or(Ordering::Equal),
        T::Double => f64::from_le_bytes(a[..8].try_into().unwrap())
            .partial_cmp(&f64::from_le_bytes(b[..8].try_into().unwrap()))
            .unwrap_or(Ordering::Equal),
        T::Monetary => f64::from_le_bytes(a[4..12].try_into().unwrap())
            .partial_cmp(&f64::from_le_bytes(b[4..12].try_into().unwrap()))
            .unwrap_or(Ordering::Equal),
        T::Date | T::Time | T::Timestamp | T::Timestampltz | T::Timestamptz => {
            le_u32(a, 0).cmp(&le_u32(b, 0))
        }
        T::Datetime | T::Datetimeltz | T::Datetimetz => {
            (le_u32(a, 0), le_u32(a, 4)).cmp(&(le_u32(b, 0), le_u32(b, 4)))
        }
        T::Enumeration => u16::from_le_bytes([a[0], a[1]]).cmp(&u16::from_le_bytes([b[0], b[1]])),
        T::Object | T::Oid => {
            let k = |x: &[u8]| {
                (
                    i16::from_le_bytes([x[6], x[7]]),
                    i32_at(x, 0),
                    i16::from_le_bytes([x[4], x[5]]),
                )
            };
            k(a).cmp(&k(b))
        }
        T::Numeric => match (decode_image(version, d, a), decode_image(version, d, b)) {
            (Ok(Value::Numeric(x)), Ok(Value::Numeric(y))) => magnitude_cmp(&x, &y),
            _ => Ordering::Equal,
        },
        T::Char | T::Nchar | T::Varchar | T::Varnchar => {
            let coll = coll.unwrap_or(Collation::ByteCs);
            let ti = matches!(d.ty, T::Char | T::Nchar);
            let s = |x: &[u8]| -> Vec<u8> {
                match decode_image(version, d, x) {
                    Ok(
                        Value::Char(s) | Value::Nchar(s) | Value::Varchar(s) | Value::Varnchar(s),
                    ) => s,
                    _ => Vec::new(),
                }
            };
            if fixed_char_11_4(version, d) || a.first() == Some(&0xFF) || b.first() == Some(&0xFF) {
                coll.compare(&s(a), &s(b), ti)
            } else {
                coll.compare(&a[1..], &b[1..], ti)
            }
        }
        T::Bit | T::Varbit => match (decode_image(version, d, a), decode_image(version, d, b)) {
            (
                Ok(Value::Bit { bits: x, bytes: xa } | Value::Varbit { bits: x, bytes: xa }),
                Ok(Value::Bit { bits: y, bytes: ya } | Value::Varbit { bits: y, bytes: ya }),
            ) => xa.cmp(&ya).then(x.cmp(&y)),
            _ => Ordering::Equal,
        },
        _ => Ordering::Equal,
    }
}
