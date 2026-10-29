//! Little-endian fixed-width field access at a byte offset inside a page or record.
//!
//! Input: a byte slice and the field's offset. Output: the decoded integer, or the slice with the field written.
//! Source: CUBRID stores every on-disk integer in host order, and all supported hosts are x86-64 (little endian).

use crate::value::{DecodeError, DecodeResult};

macro_rules! le_field {
    ($get:ident, $try_get:ident, $put:ident, $t:ty) => {
        /// Reads the field at `off` of a buffer whose size our own code fixed (a page, a checked image).
        /// Panics if the slice is too short, like any out-of-range index.
        pub fn $get(buf: &[u8], off: usize) -> $t {
            <$t>::from_le_bytes(*buf[off..].first_chunk().expect("field inside the buffer"))
        }

        /// Reads the field at `off` of bytes from a volume; an error, not a panic, when it runs past the end.
        pub fn $try_get(buf: &[u8], off: usize) -> DecodeResult<$t> {
            buf.get(off..)
                .and_then(|s| s.first_chunk())
                .map(|b| <$t>::from_le_bytes(*b))
                .ok_or_else(|| DecodeError {
                    at: off,
                    what: format!(
                        "{}-byte field past the end of {} bytes",
                        size_of::<$t>(),
                        buf.len()
                    ),
                })
        }

        /// Writes the field at `off`.
        pub fn $put(buf: &mut [u8], off: usize, v: $t) {
            buf[off..off + size_of::<$t>()].copy_from_slice(&v.to_le_bytes());
        }
    };
}

le_field!(get_i16, try_get_i16, put_i16, i16);
le_field!(get_u16, try_get_u16, put_u16, u16);
le_field!(get_i32, try_get_i32, put_i32, i32);
le_field!(get_u32, try_get_u32, put_u32, u32);
le_field!(get_i64, try_get_i64, put_i64, i64);
le_field!(get_u64, try_get_u64, put_u64, u64);
le_field!(get_f32, try_get_f32, put_f32, f32);
le_field!(get_f64, try_get_f64, put_f64, f64);
