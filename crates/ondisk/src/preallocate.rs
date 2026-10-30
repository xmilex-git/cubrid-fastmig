//! Allocates a file's space on its filesystem up front, so later writes into it cannot fail for want of space.
//!
//! Input: an open file and a length. Output: the blocks of `[0, len)` allocated (the file grows to `len`).
//! Used for the spill arena, the byte-copied target volumes and the new log, which a run takes before its first write;
//! [`unshare`] for the pages a run rewrites in a cloned volume
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

unsafe extern "C" {
    fn fallocate(fd: i32, mode: i32, offset: i64, len: i64) -> i32;
}

/// Linux `FALLOC_FL_UNSHARE_RANGE`.
const FALLOC_FL_UNSHARE_RANGE: i32 = 0x40;

/// Gives `[off, off + len)` of `f` blocks of its own where it shares them with another file (a reflink clone), so
/// later writes there take no new space (workspace#526: the pages a run rewrites in a cloned volume, taken at the
/// start). XFS only; other filesystems refuse (`EOPNOTSUPP`).
pub fn unshare(f: &File, off: u64, len: u64) -> io::Result<()> {
    if len == 0 {
        return Ok(());
    }
    let off = i64::try_from(off).map_err(io::Error::other)?;
    let len = i64::try_from(len).map_err(io::Error::other)?;
    // SAFETY: the descriptor stays open for the whole call; non-negative offset and length are valid arguments
    let rc = unsafe { fallocate(f.as_raw_fd(), FALLOC_FL_UNSHARE_RANGE, off, len) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
