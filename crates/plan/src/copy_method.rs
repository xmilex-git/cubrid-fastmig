//! The copy method of one source volume or asset: clone when the target is on the source's filesystem, copy
//! otherwise.
//!
//! Input: the source file, the target directory (it may not exist yet) and `--no-clone`. Output: the planned
//! [`CopyMethod`]. The copy step still tries FICLONE and falls back to copy when the filesystem refuses (a filesystem
//! without reflink), so the plan names what is tried first.
//! Source: decision G1 workspace#481 (clone whenever the filesystem allows it, no threshold).

use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyMethod {
    Clone,
    Copy,
}

impl CopyMethod {
    pub fn name(self) -> &'static str {
        match self {
            CopyMethod::Clone => "clone",
            CopyMethod::Copy => "copy",
        }
    }
}

/// The filesystem device of `p`, or of its nearest existing ancestor.
pub fn device_of(p: &Path) -> io::Result<u64> {
    let mut at = p;
    loop {
        match at.metadata() {
            Ok(m) => return Ok(m.dev()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                at = at.parent().ok_or(e)?;
            }
            Err(e) => return Err(e),
        }
    }
}

pub fn predict(source: &Path, target_dir: &Path, allow_clone: bool) -> io::Result<CopyMethod> {
    Ok(
        if allow_clone && device_of(source)? == device_of(target_dir)? {
            CopyMethod::Clone
        } else {
            CopyMethod::Copy
        },
    )
}
