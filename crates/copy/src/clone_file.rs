//! Reflink clone of one whole file, the first choice of copy method.
//!
//! Input: an open source file and an empty target file opened for writing. Output: `true` when the target now shares
//! the source's extents, `false` when this filesystem pair cannot share them (another filesystem, no reflink), so the
//! caller copies bytes instead.
//! Source: Linux `ioctl_ficlone(2)` (`FICLONE` = `_IOW(0x94, 9, int)`); decision G1 workspace#481.

use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;

const FICLONE: u64 = 0x4004_9409;
const EXDEV: i32 = 18;
const EINVAL: i32 = 22;
const ENOTTY: i32 = 25;
const EOPNOTSUPP: i32 = 95;

unsafe extern "C" {
    fn ioctl(fd: i32, request: u64, ...) -> i32;
}

/// Clones `src` into `dst` whenever the filesystem allows it (workspace#481); any other failure is an error.
pub fn try_clone(src: &File, dst: &File) -> io::Result<bool> {
    // SAFETY: FICLONE reads only its integer argument (the source fd); no memory of ours is passed.
    let rc = unsafe { ioctl(dst.as_raw_fd(), FICLONE, src.as_raw_fd()) };
    if rc == 0 {
        return Ok(true);
    }
    let err = io::Error::last_os_error();
    match err.raw_os_error() {
        Some(EXDEV | EINVAL | ENOTTY | EOPNOTSUPP) => Ok(false),
        _ => Err(err),
    }
}
