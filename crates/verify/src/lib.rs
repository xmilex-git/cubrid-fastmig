//! Verifier of a migrated database (and of its source): heap chains, B-tree structure and index ↔ heap (key, OID)
//! consistency with foreign keys, catalog references and OBJECT values, sector ownership, LOB files — O(n log n),
//! sector- and index-parallel, under the run's worker and memory limits. It replaces the engine's `checkdb`, whose
//! per-row index probe is O(n²) on low-cardinality keys (ADR 0007).
//!
//! Entry points: [`database::Database::open`], [`run::verify_database`], [`classify::classify`], [`report`].
//! Source: G3 workspace#483 (checks 1–4, report, exit codes), G6 workspace#488 (check 5), G5 workspace#486
//! (deferred indexes), G2 workspace#482 (measured header statistics); ticket P1f workspace#495.
//!
//! Names follow GLOSSARY.md terms (AGENTS.md § Files and modules).

pub mod btree_walk;
pub mod catalog_refs;
pub mod classify;
pub mod database;
pub mod defect;
pub mod foreign_key;
pub mod heap_chain;
pub mod heap_pass;
pub mod index_merge;
pub mod live_row_map;
pub mod lob_files;
pub mod merge_shards;
pub mod object_refs;
pub mod report;
pub mod run;
pub mod sector_owner;

pub use classify::{Classified, DefectOrigin, classify};
pub use database::{Database, Role};
pub use defect::{Check, Defect};
pub use run::{DbResult, Limits, verify_database};
