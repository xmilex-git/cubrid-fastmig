//! Moves a source database's files to the target: per-volume clone or byte copy under the worker limit, the source
//! read lock, the incomplete marker, out-of-volume assets (LOB files, Java SP directories), the new active log and
//! the target registration (`_vinf`, `databases.txt`).
//!
//! Names follow the GLOSSARY.md terms; its _Avoid_ words are not used.

pub mod byte_copy;
pub mod clone_file;
pub mod durable;
pub mod incomplete_marker;
pub mod lob_manifest;
pub mod new_log;
pub mod new_log_start;
pub mod out_of_volume_asset;
pub mod register_db;
pub mod source_lock;
pub mod volinfo;
pub mod volume_copy;
pub mod volume_header_transform;
pub mod volume_paths;
pub mod worker_pool;

pub use byte_copy::{AsIs, InlineTreatment};
pub use clone_file::try_clone;
pub use incomplete_marker::IncompleteMarker;
pub use source_lock::{open_locked_log, open_source};
pub use volume_copy::{CopyMethod, VolumeJob, VolumeOutcome, copy_volumes};
