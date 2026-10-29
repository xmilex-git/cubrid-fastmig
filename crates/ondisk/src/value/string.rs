//! Character string disk formats: the VARCHAR string (both versions, and CHAR in develop) and the 11.4 fixed CHAR.
//!
//! Input: OR bytes at a cursor. Output: the string bytes (decompressed), or the encoded bytes.
//! Source: engine `src/object/object_primitive.c` `pr_write_uncompressed_string_to_buffer`,
//! `pr_write_compressed_string_to_buffer`, `mr_readval_string_internal`, `mr_writeval_char_internal`
//! (develop @5b1867174 :14650-14980, :10926, :12136); 11.4 fixed CHAR `mr_readval_char_internal`
//! (release/11.4 @46212adcd) and `intl_char_size` (`src/base/intl_support.c`); R2 § 2.1, § 2.3.

use crate::VersionLine;
use crate::value::lzo1x;
use crate::value::or_buf::{DecodeError, DecodeResult, OrReader, OrWriter};

/// Engine `OR_MINIMUM_STRING_LENGTH_FOR_COMPRESSION`: strings at least this long use the 0xFF header.
pub const MIN_LENGTH_FOR_COMPRESSION: usize = 255;

/// Engine `INTL_CODESET` values stored in domains.
pub mod codeset {
    pub const ASCII: u8 = 0;
    pub const RAW_BITS: u8 = 1;
    pub const RAW_BYTES: u8 = 2;
    pub const ISO88591: u8 = 3;
    pub const KSC5601_EUC: u8 = 4;
    pub const UTF8: u8 = 5;
}

/// Engine `INTL_CODESET_MULT`: bytes per character reserved by a fixed CHAR(n).
pub fn codeset_mult(cs: u8) -> usize {
    match cs {
        codeset::UTF8 => 4,
        codeset::KSC5601_EUC => 3,
        _ => 1,
    }
}

/// Engine `STR_SIZE(prec, codeset)`: bytes of a fixed CHAR(prec)/NCHAR(prec)/BIT(prec) field.
pub fn str_size(precision: i32, cs: u8) -> usize {
    let p = precision.max(0) as usize;
    if cs == codeset::RAW_BITS {
        p.div_ceil(8)
    } else {
        codeset_mult(cs) * p
    }
}

/// Engine `intl_char_size`: bytes taken by the first `chars` characters of `s` in codeset `cs`, capped at `s.len()`.
pub fn char_prefix_len(s: &[u8], chars: usize, cs: u8) -> usize {
    let mut pos = 0;
    for _ in 0..chars {
        if pos >= s.len() {
            break;
        }
        let b = s[pos];
        let w = match cs {
            // `intl_Len_utf8_char`: lead byte decides the width; stray continuation bytes count as 1.
            codeset::UTF8 => match b {
                0xC0..=0xDF => 2,
                0xE0..=0xEF => 3,
                0xF0..=0xF7 => 4,
                0xF8..=0xFB => 5,
                0xFC..=0xFD => 6,
                _ => 1,
            },
            // `intl_nextchar_euc`: ASCII 1, SS3 (0x8F) 3, other 8-bit 2.
            codeset::KSC5601_EUC => match b {
                0x00..=0x7F => 1,
                0x8F => 3,
                _ => 2,
            },
            _ => 1,
        };
        pos += w;
    }
    pos.min(s.len())
}

/// The block format of a compressed string's payload: LZO1X in 10.2, LZ4 from 11.0 (CBRD-23703 ecaa13201; R8 § 2.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StringCompression {
    Lzo1x,
    Lz4,
}

impl StringCompression {
    /// The format a line's writer used; this tool writes LZ4 for every target (workspace#520).
    pub fn of(line: VersionLine) -> Self {
        match line {
            VersionLine::V10_2 => StringCompression::Lzo1x,
            VersionLine::V11_4 | VersionLine::Develop => StringCompression::Lz4,
        }
    }

    /// Decompresses `block` to exactly `len` bytes; the error names the format and the offset into the block.
    pub fn decompress(self, block: &[u8], len: usize) -> Result<Vec<u8>, String> {
        match self {
            StringCompression::Lz4 => {
                let mut out = vec![0u8; len];
                let n = lz4_flex::block::decompress_into(block, &mut out)
                    .map_err(|e| format!("LZ4: {e}"))?;
                if n != len {
                    return Err(format!("LZ4 gave {n} bytes, header says {len}"));
                }
                Ok(out)
            }
            StringCompression::Lzo1x => lzo1x::decompress(block, len)
                .map_err(|e| format!("LZO1X at block byte {}: {}", e.at, e.what)),
        }
    }
}

/// Reads one VARCHAR-format string (engine `mr_readval_string_internal` with `INT_ALIGNMENT`):
/// `[len:u8][bytes][0x00]` or `[0xFF][compressed_len:BE i32][len:BE i32][LZ4/LZO1X or raw][0x00]`, then 4-byte
/// alignment. The cursor ends after the padding. Returns the decompressed bytes.
pub fn read_string(r: &mut OrReader, compression: StringCompression) -> DecodeResult<Vec<u8>> {
    let first = r.u8()?;
    let out = if first < 0xFF {
        r.bytes(first as usize)?.to_vec()
    } else {
        let clen = r.i32()?;
        let len = r.i32()?;
        if clen < 0 || len < 0 {
            return Err(r.error(format!("bad string lengths {clen}/{len}")));
        }
        if clen > 0 {
            let at = r.pos();
            let src = r.bytes(clen as usize)?;
            compression
                .decompress(src, len as usize)
                .map_err(|what| DecodeError { at, what })?
        } else {
            r.bytes(len as usize)?.to_vec()
        }
    };
    // trailing NUL then `or_get_align32`
    r.skip(1)?;
    r.align4()?;
    Ok(out)
}

/// Convenience for callers holding a whole buffer: the string at `off` and the bytes it used (padding included).
/// Alignment is counted from the start of `buf`, so `buf` must start where the engine's buffer was 4-byte aligned
/// (a record or packed value start).
pub fn read_varchar(
    buf: &[u8],
    off: usize,
    compression: StringCompression,
) -> DecodeResult<(Vec<u8>, usize)> {
    let mut r = OrReader::new(buf);
    r.skip(off)?;
    let s = read_string(&mut r, compression)?;
    Ok((s, r.pos() - off))
}

/// Writes one VARCHAR-format string the way develop does (engine `mr_writeval_string_internal` /
/// `mr_writeval_char_internal`): below 255 bytes the short header; from 255 the 0xFF header with an LZ4 block when
/// it is shorter than `len - 8`, the raw bytes otherwise. Same bytes for 11.4 VARCHAR (R2 § 2).
// compress like develop's writer; byte equality with the engine is not required, only a valid LZ4 block (workspace#482)
pub fn write_string(w: &mut OrWriter, s: &[u8]) {
    if s.len() < MIN_LENGTH_FOR_COMPRESSION {
        w.u8(s.len() as u8);
        w.bytes(s);
    } else {
        let c = lz4_flex::block::compress(s);
        w.u8(0xFF);
        if c.len() < s.len() - 8 {
            w.i32(c.len() as i32);
            w.i32(s.len() as i32);
            w.bytes(&c);
        } else {
            w.i32(0);
            w.i32(s.len() as i32);
            w.bytes(s);
        }
    }
    w.u8(0);
    w.align4();
}

/// Reads an 11.4 fixed CHAR(precision)/NCHAR(precision) field of `str_size(precision, cs)` bytes and returns its
/// first `precision` characters (engine 11.4 `mr_readval_char_internal`; the writer space-padded the field, so these
/// characters are the same bytes develop's writer pads to before encoding, R2 § 2.3).
pub fn read_fixed_char_11_4(r: &mut OrReader, precision: i32, cs: u8) -> DecodeResult<Vec<u8>> {
    let field = r.bytes(str_size(precision, cs))?;
    let n = char_prefix_len(field, precision.max(0) as usize, cs);
    Ok(if n == 0 {
        field.to_vec()
    } else {
        field[..n].to_vec()
    })
}

/// Writes an 11.4 fixed CHAR(precision) field: the bytes, space-padded to `str_size` (engine 11.4
/// `mr_writeval_char_internal` pads with `' '`).
pub fn write_fixed_char_11_4(w: &mut OrWriter, s: &[u8], precision: i32, cs: u8) {
    let size = str_size(precision, cs);
    let n = s.len().min(size);
    w.bytes(&s[..n]);
    w.buf.resize(w.buf.len() + (size - n), b' ');
}
