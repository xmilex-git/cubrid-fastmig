//! In-place rewrite of 11.4 heap files into develop's format, and of develop heap files on the same-version path
//! (workspace#501): vacuum-residue removal (patch), re-encoded record placement (same slot, REC_RELOCATION +
//! NEWHOME, REC_BIGONE + overflow), and the develop header page with its bestspace pages.
//!
//! Input: a writable [`crate::volume_set::VolumeSet`] holding a copy of the source, through
//! [`crate::file_alloc::SpaceManager`]. Output: each heap rewritten, with a [`HeapReport`].
//! Source: R2 (workspace#474) § 1.4-1.9, § 3 R2-R7; G2 (workspace#482) decisions 6, 7, 10; workspace#499.

pub mod header_rewrite;
pub mod heap_state;
pub mod overflow;
pub mod page_rewrite;
pub mod placement;
pub mod re_encode;
pub mod record_patch;
pub mod rewrite_heap;
pub mod vacuum_horizon;

pub use page_rewrite::PageCounts;
pub use placement::max_slotted_len;
pub use re_encode::{ClassReEncoder, ReEncode, mvcc_disabled_class};
pub use rewrite_heap::{HeapJob, HeapReport, rewrite_heap};
pub use vacuum_horizon::VacuumHorizon;
