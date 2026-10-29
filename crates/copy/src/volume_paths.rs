//! Rewrites the path chain in the target volume headers: each volume's own full name and its successor's, so the
//! engine never follows a header back to a source volume.
//!
//! Input: the target volume paths in volid order and the IO page size. Output: page 0 of each target volume
//! rewritten in place (only the `var_fields` strings and their offsets change; the page LSA does not).
//! Source: engine `disk_manager.c:disk_vhdr_set_vol_fullname`, `disk_vhdr_set_next_vol_fullname` (develop:5328-5355);
//! decision G1 workspace#481 (the target volume headers carry target paths).

use std::fs::OpenOptions;
use std::io;
use std::os::unix::fs::FileExt;
use std::path::PathBuf;

use fastmig_ondisk::volume_header::{VolumeHeader, set_paths};

pub fn rewrite(targets: &[PathBuf], io_pagesize: usize) -> io::Result<()> {
    for (i, path) in targets.iter().enumerate() {
        let f = OpenOptions::new().read(true).write(true).open(path)?;
        let mut page = vec![0u8; io_pagesize];
        f.read_exact_at(&mut page, 0)?;
        let next = match targets.get(i + 1) {
            Some(n) => n
                .to_str()
                .ok_or_else(|| io::Error::other("non-UTF-8 path"))?
                .to_string(),
            // the last volume keeps whatever it recorded; the engine does not follow it (next_volid is -1)
            None => VolumeHeader::read(&page)?.next_vol_fullname,
        };
        let own = path
            .to_str()
            .ok_or_else(|| io::Error::other("non-UTF-8 path"))?;
        set_paths(&mut page, own, &next)?;
        f.write_all_at(&page, 0)?;
    }
    Ok(())
}
