//! The migration plan: what `migrate` will do, decided before anything is written — the version pair, each volume's
//! copy method, each class's page treatment grade, the deferred indexes, the LOB manifest, and the pre-check.
//!
//! Names follow GLOSSARY.md terms (AGENTS.md § Files and modules).

pub mod copy_method;
pub mod free_space;
pub mod lob_scan;
pub mod migration_plan;
pub mod version_pair;

pub use copy_method::CopyMethod;
pub use lob_scan::{LobScan, ReferencedLobFile};
pub use migration_plan::{HeapTreatment, PageTreatment, Plan, PlanInputs, Refused, build};
pub use version_pair::VersionPair;
