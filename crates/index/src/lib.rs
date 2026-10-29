//! Parallel B-tree rebuild of every index in the target database: the engine's no-logging parallel load path
//! (R6, workspace#478) with the MSD radix run sort of CUBRID/cubrid#8209, writing develop-layout B-tree files.
//!
//! Names follow GLOSSARY.md terms (AGENTS.md § Files and modules).

pub mod bt_stats;
pub mod btree_file;
pub mod btree_page;
pub mod build;
pub mod build_error;
pub mod build_many;
pub mod fill;
pub mod heap_keys;
pub mod heap_scan;
pub mod index_def;
pub mod index_shape;
pub mod job_pool;
pub mod key_domain_order;
pub mod key_order;
pub mod kway_merge;
pub mod leaf_builder;
pub mod memory_limit;
pub mod nonleaf_builder;
pub mod ovf_key_file;
pub mod ovf_key_writer;
pub mod ovf_oid_chain;
pub mod page_sink;
pub mod pair_count;
pub mod rebuild;
pub mod run_writer;
pub mod scratch_sink;
pub mod sort_run;
pub mod spill_arena;
pub mod spill_dir;
pub mod spill_run;
pub mod splitter;
pub mod temp_disk;
pub mod volume_sink;
pub mod worker_limit;

pub use build::{BuiltIndex, IndexJob, build_index};
pub use build_error::BuildError;
pub use build_many::build_indexes;
pub use fill::{FilledIndex, Limits, fill_btrees};
pub use index_def::{IndexDef, IndexOwner};
pub use index_shape::IndexShape;
pub use key_order::KeyOrder;
pub use memory_limit::MemoryLimit;
pub use page_sink::PageSink;
pub use rebuild::{RebuiltIndexes, TargetClass, rebuild_indexes};
pub use run_writer::{IndexRuns, RunWriter};
pub use sort_run::Run;
pub use spill_dir::SpillDir;
pub use worker_limit::WorkerLimit;
