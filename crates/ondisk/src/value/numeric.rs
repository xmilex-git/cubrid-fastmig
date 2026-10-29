//! NUMERIC disk formats: 11.4's fixed 16-byte two's complement and develop's 3-byte header plus magnitude.
//!
//! Input: OR bytes at a cursor and the column's precision and scale. Output: [`Numeric`] (sign, 17-byte magnitude,
//! precision, scale), or its bytes in either version.
//! Source: release/11.4 @46212adcd `src/object/object_primitive.c` `mr_data_readval_numeric` (16 B,
//! `DB_NUMERIC_BUF_SIZE`), `src/query/numeric_opfunc.c:166-169,597-612` (sign, negation); develop @5b1867174
//! `object_primitive.c:117-157` (header layout, size tables), `:8722-8845` (`mr_data_writeval_numeric`,
//! `mr_data_readval_numeric`), `:8945` (`mr_get_fixed_numeric_size`); R2 § 2.2 (CBRD-26006 #6486).

use crate::value::domain::DEFAULT_NUMERIC_PRECISION_DEVELOP;
use crate::value::or_buf::{DecodeResult, OrReader, OrWriter};

/// Engine develop `NUMERIC_HEADER_SIZE`.
const HEADER_SIZE: usize = 3;
/// Engine develop `_gv_mr_fixed_numeric_bytes_to_size`: significant magnitude bytes → disk size.
const BYTES_TO_SIZE: [usize; 17] = [0, 4, 8, 8, 8, 8, 12, 12, 12, 12, 16, 16, 16, 16, 20, 20, 20];
/// Engine develop `_gv_mr_float_numeric_precision_to_size` (precision 40 = floating NUMERIC).
const FLOAT_PRECISION_TO_SIZE: [usize; 41] = [
    0, 4, 4, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 12, 12, 12, 12, 12, 12, 12, 12, 12, 16, 16, 16, 16, 16,
    16, 16, 16, 16, 16, 20, 20, 20, 20, 20, 20, 20, 20, 20,
];

/// A NUMERIC value: unscaled integer as sign + magnitude, with the precision and scale it is read under.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Numeric {
    pub negative: bool,
    /// Big-endian magnitude, develop's 17-byte `DB_C_NUMERIC` buffer.
    pub magnitude: [u8; 17],
    pub precision: i32,
    pub scale: i32,
    /// Engine develop floating-NUMERIC mark of a `DB_VALUE`: read under a domain of `DB_DEFAULT_NUMERIC_PRECISION`.
    /// The writer sizes such a value by its precision, not by its magnitude. Never set for an 11.4 value.
    pub floating: bool,
}

impl Numeric {
    /// Significant magnitude bytes after byte 0, 1 for zero (engine `mr_get_fixed_numeric_size`).
    fn significant_bytes(&self) -> usize {
        let lead = self.magnitude[1..].iter().take_while(|b| **b == 0).count();
        (16 - lead).max(1)
    }
}

/// Reads the 11.4 fixed 16-byte NUMERIC (big-endian two's complement of the unscaled value).
pub fn read_numeric_11_4(r: &mut OrReader, precision: i32, scale: i32) -> DecodeResult<Numeric> {
    let b: [u8; 16] = r.bytes(16)?.try_into().unwrap();
    let v = i128::from_be_bytes(b);
    let mut magnitude = [0u8; 17];
    magnitude[1..].copy_from_slice(&v.unsigned_abs().to_be_bytes());
    Ok(Numeric {
        negative: v < 0,
        magnitude,
        precision,
        scale,
        floating: false,
    })
}

/// Writes the 11.4 fixed 16-byte NUMERIC. Panics if the magnitude does not fit 16 bytes (11.4 precision ≤ 38 does).
pub fn write_numeric_11_4(w: &mut OrWriter, n: &Numeric) {
    assert_eq!(n.magnitude[0], 0, "NUMERIC magnitude exceeds 16 bytes");
    let m = u128::from_be_bytes(n.magnitude[1..].try_into().unwrap());
    let v = if n.negative {
        (m as i128).wrapping_neg()
    } else {
        m as i128
    };
    w.bytes(&v.to_be_bytes());
}

/// Reads a develop NUMERIC: `[size | sign<<7][precision | scale_sign<<7][|scale|]` then the low `size - 3` magnitude
/// bytes. Precision and scale come from the header; the value is floating when `domain_precision` is the default
/// precision (`mr_data_readval_numeric`).
pub fn read_numeric_develop(r: &mut OrReader, domain_precision: i32) -> DecodeResult<Numeric> {
    let at = r.pos();
    let h = r.bytes(HEADER_SIZE)?;
    let size = (h[0] & 0x7F) as usize;
    let negative = h[0] & 0x80 != 0;
    let precision = (h[1] & 0x7F) as i32;
    let scale = if h[1] & 0x80 != 0 {
        -(h[2] as i32)
    } else {
        h[2] as i32
    };
    if !(HEADER_SIZE + 1..=20).contains(&size) {
        return Err(r.error(format!("NUMERIC size {size} at {at}")));
    }
    let floating = domain_precision == DEFAULT_NUMERIC_PRECISION_DEVELOP;
    if floating && precision as usize >= FLOAT_PRECISION_TO_SIZE.len() {
        return Err(r.error(format!("floating NUMERIC precision {precision} at {at}")));
    }
    let data = r.bytes(size - HEADER_SIZE)?;
    let mut magnitude = [0u8; 17];
    magnitude[17 - data.len()..].copy_from_slice(data);
    Ok(Numeric {
        negative,
        magnitude,
        precision,
        scale,
        floating,
    })
}

/// Writes a develop NUMERIC (engine `mr_data_writeval_numeric`); zero is written as positive.
pub fn write_numeric_develop(w: &mut OrWriter, n: &Numeric) {
    let size = if n.floating {
        FLOAT_PRECISION_TO_SIZE[n.precision as usize]
    } else {
        BYTES_TO_SIZE[n.significant_bytes()]
    };
    let is_zero = n.magnitude.iter().all(|b| *b == 0);
    let sign = if n.negative && !is_zero { 0x80 } else { 0 };
    w.u8(size as u8 | sign);
    if n.scale < 0 {
        w.u8(n.precision as u8 | 0x80);
        w.u8((-n.scale) as u8);
    } else {
        w.u8(n.precision as u8);
        w.u8(n.scale as u8);
    }
    w.bytes(&n.magnitude[17 - (size - HEADER_SIZE)..]);
}
