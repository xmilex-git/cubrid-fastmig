//! Transforms page 0 of every target volume copied from a 10.2..11.3 source to the 136 B volume header layout, before
//! anything opens the target as a volume set.
//!
//! Input: the target volume paths, the IO page size, and the `vol_creation` to fill in. Output: page 0 of each
//! target volume rewritten in place.
//! Source: R8 § 2.3 (128 B → 136 B, CBRD-25365 44c022c31); `vol_creation` is the source `db_creation`, the value the
//! new log also carries (workspace#522, #529).

use std::fs::OpenOptions;
use std::io;
use std::os::unix::fs::FileExt;
use std::path::PathBuf;

use fastmig_ondisk::volume_header::transform_from_before_11_4;

pub fn transform(targets: &[PathBuf], io_pagesize: usize, vol_creation: i64) -> io::Result<()> {
    for path in targets {
        let f = OpenOptions::new().read(true).write(true).open(path)?;
        let mut page = vec![0u8; io_pagesize];
        f.read_exact_at(&mut page, 0)?;
        transform_from_before_11_4(&mut page, vol_creation)
            .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))?;
        f.write_all_at(&page, 0)?;
    }
    Ok(())
}
