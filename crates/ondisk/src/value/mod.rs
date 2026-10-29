//! Values as the engine stores them inside records: type ids, domains, and per-type disk codecs for 11.4 and develop.
//!
//! Input: OR-encoded bytes (big endian) plus the domain or a domain tag. Output: decoded values, or the bytes of a
//! value in a chosen version's encoding.
//! Source: engine `src/object/object_primitive.c`, `src/object/object_representation.c` (release/11.4 @46212adcd,
//! develop @5b1867174); R2 § 2.

pub mod codec;
pub mod db_type;
pub mod db_value;
pub mod develop_map;
pub mod domain;
pub mod elo;
pub mod json;
pub mod numeric;
pub mod or_buf;
pub mod packed_domain;
pub mod set;
pub mod string;

pub use codec::{decode_data, decode_tagged, decode_value, encode_data, encode_tagged};
pub use db_type::DbType;
pub use db_value::{SetElement, SetValue, Value};
pub use develop_map::{changes_in_develop, domain_to_develop, value_to_develop};
pub use domain::{CollationFlag, Domain};
pub use elo::{Elo, LobPath};
pub use numeric::Numeric;
pub use or_buf::{DecodeError, DecodeResult, OrReader, OrWriter};
pub use packed_domain::{decode_packed_domain, encode_packed_domain};
pub use set::{decode_set, encode_set};
pub use string::{read_string, read_varchar, write_string};
