//! The representation directory (catalog file, `PAGE_CATALOG` pages): per class one directory record, one class-info
//! record and one disk-representation record, all big endian.
//!
//! Input: catalog record bytes (a disk-representation record as its fragments across overflow pages).
//! Output: [`ClsInfo`], [`ReprDir`], [`DiskRepr`], [`CatalogPageHeader`], and their develop encodings.
//! Source: engine `src/storage/system_catalog.c` (release/11.4 @46212adcd, develop @5b1867174); R3 § 3.

mod cls_info;
mod disk_repr;
mod obj_ids;
mod page_header;
mod repr_dir;

pub use cls_info::ClsInfo;
pub use disk_repr::{BtStats, DiskAttr, DiskRepr, PKEYS_NUM};
pub use page_header::{CATALOG_HEADER_SLOT, CATALOG_MAX_SLOT_ID_SIZE, CatalogPageHeader};
pub use repr_dir::{ReprDir, ReprItem};
