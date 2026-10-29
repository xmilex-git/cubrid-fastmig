//! Values as the engine stores them inside records: type ids, domains, and per-type disk codecs for 11.4 and develop.
//!
//! Input: OR-encoded bytes (big endian) plus the domain or a domain tag. Output: decoded values, or the bytes of a
//! value in a chosen version's encoding.
//! Source: engine `src/object/object_primitive.c`, `src/object/object_representation.c` (release/11.4 @46212adcd,
//! develop @5b1867174); R2 § 2.

pub mod db_type;
pub mod domain;
pub mod or_buf;
pub mod packed_domain;
pub mod string;

pub use db_type::DbType;
pub use domain::{CollationFlag, Domain};
pub use or_buf::{DecodeError, DecodeResult, OrReader, OrWriter};
pub use packed_domain::{decode_packed_domain, encode_packed_domain};
pub use string::{read_string, read_varchar, write_string};
