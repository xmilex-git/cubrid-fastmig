//! Regenerates the system catalog (class objects, representation directory, _db_* tables) in the target format.

pub mod classify;
pub mod dry_run;
pub mod source_classes;
pub mod system_classes;
pub mod template;
pub mod template_extract;
pub mod template_store;
