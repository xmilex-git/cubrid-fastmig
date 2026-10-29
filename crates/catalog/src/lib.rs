//! Regenerates the system catalog (class objects, representation directory, _db_* tables) in the target format.

pub mod attr_default;
pub mod auth_rows;
pub mod class_rewrite;
pub mod class_rows;
pub mod classify;
pub mod ct_index_rows;
pub mod ct_rows;
pub mod disk_repr_build;
pub mod dry_run;
pub mod output_sql;
pub mod row_encode;
pub mod same_slot_rows;
pub mod serial_name;
pub mod source_classes;
pub mod system_classes;
pub mod target_classes;
pub mod template;
pub mod template_class;
pub mod template_extract;
pub mod template_store;
