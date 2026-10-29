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
use crate::le_field::{
    get_f32, get_f64, get_i16, get_i32, get_i64, get_u16, get_u32, try_get_f64, try_get_i16,
    try_get_i64, try_get_u16, try_get_u32,
};
use crate::value::db_type::DbType;
use crate::value::db_value::Value;
use crate::value::domain::{Domain, FLOATING_PRECISION};
use crate::value::numeric::{
    Numeric, read_numeric_11_4, read_numeric_develop, write_numeric_11_4, write_numeric_develop,
};
use crate::value::or_buf::{DecodeError, DecodeResult, OrReader, OrWriter};
use crate::value::string::{MIN_LENGTH_FOR_COMPRESSION, str_size};

/// Built-in collations whose order this module implements (engine `LANG_COLL_*` ids, the same in 11.4 and
/// develop, `src/base/language_support.h:107-116`). LDML locale collations (ids from 32) are not implemented:
/// [`Collation::is_supported`] lets a pre-check refuse indexes that use them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Collation {
    /// `lang_fastcmp_byte` with the identity weights: iso88591_bin (0), utf8_bin (1), iso88591_en_cs (2),
    /// utf8_en_cs (4).
    ByteCs,
    /// `lang_fastcmp_byte` with 'a'..'z' weighed as 'A'..'Z': iso88591_en_ci (3), utf8_en_ci (5).
    ByteCi,
    /// `lang_strcmp_utf8` (code points) with the EN_cs weights: utf8_ko_cs (7).
    Utf8En,
    /// `lang_strcmp_utf8` with the Turkish weights of `lang_init_coll_Utf8_tr_cs`: utf8_tr_cs (6).
    Utf8Tr,
    /// `lang_fastcmp_ko`: euckr_bin (8).
    EucKr,
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

/// Engine `LANG_CHAR_COUNT_TR`: code points the Turkish table weighs.
const TR_COUNT: usize = 352;

/// The weights `lang_init_coll_Utf8_tr_cs` builds: identity, then each Turkish letter moved right after its Latin
/// base letter, the weights between shifted up by one.
fn tr_weights() -> &'static [u32; TR_COUNT] {
    static W: std::sync::OnceLock<[u32; TR_COUNT]> = std::sync::OnceLock::new();
    W.get_or_init(|| {
        let mut w = [0u32; TR_COUNT];
        for (i, x) in w.iter_mut().enumerate() {
            *x = i as u32;
        }
        let upper = [
            (0xC7, b'C'),
            (0x11E, b'G'),
            (0x130, b'I'),
            (0xD6, b'O'),
            (0x15E, b'S'),
            (0xDC, b'U'),
        ];
        let lower = [
            (0xE7, b'c'),
            (0x11F, b'g'),
            (0x131, b'h'),
            (0xF6, b'o'),
            (0x15F, b's'),
            (0xFC, b'u'),
        ];
        for (cp, prev) in upper.into_iter().chain(lower) {
            let repl = prev as usize + 1;
            let w_repl = w[repl];
            w[cp] = w_repl;
            for x in w.iter_mut().take(cp).skip(repl) {
                if *x >= w_repl {
                    *x += 1;
                }
            }
        }
        w
    })
}

impl Collation {
    /// The order of collation `id`, or `None` for an LDML locale collation (id 32 and up) or an unknown id.
    pub fn from_id(id: i32) -> Option<Collation> {
        match id {
            0 | 1 | 2 | 4 => Some(Collation::ByteCs),
            3 | 5 => Some(Collation::ByteCi),
            6 => Some(Collation::Utf8Tr),
            7 => Some(Collation::Utf8En),
            8 => Some(Collation::EucKr),
            9 => Some(Collation::Binary),
            _ => None,
        }
    }

    /// Whether keys of collation `id` can be ordered here (a plan pre-check refuses indexes on the rest).
    pub fn is_supported(id: i32) -> bool {
        Self::from_id(id).is_some()
    }

    /// `lang_strmatch_utf8` without pattern matching: a code point below the table size by its weight, above it by
    /// itself, a space 0; with `ti`, a tail of code points whose `weights_ti` are 0 does not count.
    fn compare_utf8(self, a: &[u8], b: &[u8], ti: bool) -> Ordering {
        let tr = self == Collation::Utf8Tr;
        let w = |cp: u32| -> u32 {
            if cp == b' ' as u32 {
                0
            } else if tr && (cp as usize) < TR_COUNT {
                tr_weights()[cp as usize]
            } else {
                // EN_cs weights: the identity below 256, the code point above
                cp
            }
        };
        // `weights_ti` 0: the NUL code point, and the space under EN_cs (`lang_init_coll_en_cs`), not under TR
        let zero_ti = |cp: u32| cp == 0 || (!tr && cp == b' ' as u32);
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
            return Ordering::Equal;
        }
        let (tail, longer) = if ra.is_empty() {
            (rb, Ordering::Less)
        } else {
            (ra, Ordering::Greater)
        };
        if !ti {
            return longer;
        }
        let mut k = 0;
        while k < tail.len() {
            let (cp, n) = utf8_cp(&tail[k..]);
            if !zero_ti(cp) {
                return longer;
            }
            k += n;
        }
        Ordering::Equal
    }

    /// `lang_fastcmp_ko`: bytes as they are, an ASCII space or an EUC space pair (A1 A1) weighing 0; past the common
    /// part, the first non-zero unit of the longer string decides (a space weighs 0 there only with `ti`).
    fn compare_euckr(a: &[u8], b: &[u8], ti: bool) -> Ordering {
        fn next(s: &[u8], i: &mut usize, space: u32) -> u32 {
            let c = s[*i];
            *i += 1;
            if c == b' ' {
                space
            } else if c == 0xA1 && *i < s.len() && s[*i] == 0xA1 {
                *i += 1;
                space
            } else {
                c as u32
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < a.len() && j < b.len() {
            let (x, y) = (next(a, &mut i, 0), next(b, &mut j, 0));
            if x != y {
                return x.cmp(&y);
            }
        }
        let tail_space = if ti { 0 } else { b' ' as u32 };
        let first_nonzero = |s: &[u8], mut k: usize| -> u32 {
            while k < s.len() {
                let c = next(s, &mut k, tail_space);
                if c != 0 {
                    return c;
                }
            }
            0
        };
        first_nonzero(a, i).cmp(&first_nonzero(b, j))
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
        match self {
            Collation::Utf8En | Collation::Utf8Tr => return self.compare_utf8(a, b, ti),
            Collation::EucKr => return Self::compare_euckr(a, b, ti),
            _ => {}
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

/// The header of a develop CHAR or any VARCHAR image.
struct StringHeader {
    /// Header length: 1, or 9 when the length byte says a compressed-length pair follows.
    header_len: usize,
    /// Payload bytes stored after the header.
    stored: usize,
    /// LZ4-compressed length, 0 when stored uncompressed.
    compressed_len: usize,
    /// Uncompressed length.
    len: usize,
}

fn string_header(b: &[u8]) -> DecodeResult<StringHeader> {
    let first = *b
        .first()
        .ok_or_else(|| err(0, "empty string image".into()))?;
    if (first as usize) < MIN_LENGTH_FOR_COMPRESSION {
        return Ok(StringHeader {
            header_len: 1,
            stored: first as usize,
            compressed_len: 0,
            len: first as usize,
        });
    }
    if b.len() < 9 {
        return Err(err(0, "short compressed string header".into()));
    }
    let mut r = OrReader::new(&b[1..]);
    let clen = r.i32()?.max(0) as usize;
    let len = r.i32()?.max(0) as usize;
    Ok(StringHeader {
        header_len: 9,
        stored: if clen > 0 { clen } else { len },
        compressed_len: clen,
        len,
    })
}

fn string_bytes(b: &[u8]) -> DecodeResult<Cow<'_, [u8]>> {
    let StringHeader {
        header_len: h,
        stored,
        compressed_len: clen,
        len,
    } = string_header(b)?;
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
                4 + try_get_u32(b, 0)? as usize
            } else {
                str_size(d.precision, d.codeset)
            }
        }
        T::Char | T::Varchar | T::Varnchar => {
            let s = string_header(b)?;
            s.header_len + s.stored
        }
        T::Bit => {
            if d.precision == FLOATING_PRECISION {
                4 + (try_get_u32(b, 0)? as usize).div_ceil(8)
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
                5 + (OrReader::new(&b[1..]).i32()?.max(0) as usize).div_ceil(8)
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
    let u32_at = |at: usize| try_get_u32(b, at);
    Ok(match d.ty {
        T::Integer => Value::Integer(u32_at(0)? as i32),
        T::Short => Value::Short(try_get_i16(b, 0)?),
        T::Bigint => Value::Bigint(try_get_i64(b, 0)?),
        T::Float => Value::Float(f32::from_bits(u32_at(0)?)),
        T::Double => Value::Double(try_get_f64(b, 0)?),
        T::Monetary => Value::Monetary {
            currency: u32_at(0)? as i32,
            amount: try_get_f64(b, 4)?,
        },
        T::Date => Value::Date(u32_at(0)?),
        T::Time => Value::Time(u32_at(0)?),
        T::Timestamp => Value::Timestamp(u32_at(0)?),
        T::Timestampltz => Value::Timestampltz(u32_at(0)?),
        T::Timestamptz => Value::Timestamptz {
            timestamp: u32_at(0)?,
            tz_id: u32_at(4)?,
        },
        T::Datetime => Value::Datetime {
            date: u32_at(0)?,
            time: u32_at(4)?,
        },
        T::Datetimeltz => Value::Datetimeltz {
            date: u32_at(0)?,
            time: u32_at(4)?,
        },
        T::Datetimetz => Value::Datetimetz {
            date: u32_at(0)?,
            time: u32_at(4)?,
            tz_id: u32_at(8)?,
        },
        T::Object | T::Oid => {
            let o = Oid::new(try_get_i16(b, 6)?, u32_at(0)? as i32, try_get_i16(b, 4)?);
            if d.ty == T::Object {
                Value::Object(o)
            } else {
                Value::Oid(o)
            }
        }
        T::Enumeration => Value::Enumeration(try_get_u16(b, 0)?),
        T::Numeric => Value::Numeric(match version {
            VersionLine::V11_4 => read_numeric_11_4(&mut OrReader::new(b), d.precision, d.scale)?,
            VersionLine::Develop => read_numeric_develop(&mut OrReader::new(b), d.precision)?,
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
                    bits: u32_at(0)?,
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
            let mut r = OrReader::new(b);
            let (h, bits) = match r.u8()? {
                0xFF => (5, r.i32()?.max(0) as u32),
                n => (1, n as u32),
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
/// for string types. Invariant: both images passed `image_len` for `d` (a split key column), so every fixed field
/// read below lies inside them.
pub fn compare_images(
    version: VersionLine,
    d: &Domain,
    coll: Option<Collation>,
    a: &[u8],
    b: &[u8],
) -> Ordering {
    use DbType as T;
    match d.ty {
        T::Integer => get_i32(a, 0).cmp(&get_i32(b, 0)),
        T::Short => get_i16(a, 0).cmp(&get_i16(b, 0)),
        T::Bigint => get_i64(a, 0).cmp(&get_i64(b, 0)),
        T::Float => get_f32(a, 0)
            .partial_cmp(&get_f32(b, 0))
            .unwrap_or(Ordering::Equal),
        T::Double => get_f64(a, 0)
            .partial_cmp(&get_f64(b, 0))
            .unwrap_or(Ordering::Equal),
        T::Monetary => get_f64(a, 4)
            .partial_cmp(&get_f64(b, 4))
            .unwrap_or(Ordering::Equal),
        T::Date | T::Time | T::Timestamp | T::Timestampltz | T::Timestamptz => {
            get_u32(a, 0).cmp(&get_u32(b, 0))
        }
        T::Datetime | T::Datetimeltz | T::Datetimetz => {
            (get_u32(a, 0), get_u32(a, 4)).cmp(&(get_u32(b, 0), get_u32(b, 4)))
        }
        T::Enumeration => get_u16(a, 0).cmp(&get_u16(b, 0)),
        T::Object | T::Oid => {
            let k = |x: &[u8]| (get_i16(x, 6), get_i32(x, 0), get_i16(x, 4));
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
