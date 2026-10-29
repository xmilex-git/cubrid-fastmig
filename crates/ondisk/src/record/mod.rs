//! Heap records: page-chain iteration, the record header, and decoding/encoding a record under a representation.
//!
//! Input: heap pages from a [`crate::volume_set::VolumeSet`], or one record's bytes. Output: records with their
//! OIDs, headers, and values.
//! Source: engine `src/storage/heap_file.c`, `src/base/object_representation_sr.c` (release/11.4 @46212adcd,
//! develop @5b1867174); R2 § 1.

pub mod develop_layout;
pub mod extract_key;
pub mod heap_chain;
pub mod heap_hdr_stats;
pub mod heap_scan;
pub mod layout;
pub mod mvcc_header;
pub mod record_codec;

pub use extract_key::{ExtractedKey, extract_key};
pub use heap_chain::HeapChain;
pub use heap_hdr_stats::HeapHdrStats;
pub use heap_scan::{HeapRecord, HeapScan, HomeKind, heap_records};
pub use layout::{LayoutAttr, RecordLayout, schema_domain};
pub use mvcc_header::RecordHeader;
pub use record_codec::{DecodedRecord, decode_record, encode_record};
