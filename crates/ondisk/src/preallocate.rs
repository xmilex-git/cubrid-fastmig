//! Allocates a file's space on its filesystem up front, so later writes into it cannot fail for want of space.
//!
//! Input: an open file and a length. Output: the blocks of `[0, len)` allocated (the file grows to `len`).
//! Used for the spill arena and the byte-copied target volumes, which a run takes before its first write
//! (workspace#526: no disk full during a run).
//! Source: POSIX `posix_fallocate(3)` (glibc on XFS/ext4: real allocation, not a sparse extension).

use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;

unsafe extern "C" {
    fn posix_fallocate(fd: i32, offset: i64, len: i64) -> i32;
}

pub fn allocate(f: &File, len: u64) -> io::Result<()> {
    if len == 0 {
        return Ok(());
    }
    let len = i64::try_from(len).map_err(io::Error::other)?;
    // SAFETY: the descriptor stays open for the whole call; offset 0 and a non-negative len are valid arguments
    let rc = unsafe { posix_fallocate(f.as_raw_fd(), 0, len) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(rc))
    }
}
