//! Allocates a file's space on its filesystem up front, so later writes into it cannot fail for want of space.
//!
//! Input: an open file and a length. Output: the blocks of `[0, len)` allocated (the file grows to `len`).
//! Used for the spill arena, the byte-copied target volumes and the new log, which a run takes before its first write;
//! [`zero`] for the pages of a cloned volume whose content the run does not need (workspace#526: no disk full during a
//! run; the rewritten pages are written over at the start instead, workspace#556).
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

/// Linux `FALLOC_FL_ZERO_RANGE`.
const FALLOC_FL_ZERO_RANGE: i32 = 0x10;

/// Gives `[off, off + len)` of `f` blocks of its own, unwritten (reading zeros), without copying what it held:
/// for pages whose content the run does not need — free sectors it will take, a heap's unallocated pages — where
/// a write of the source bytes would copy them (workspace#526, #556). Fails before changing anything when the filesystem
/// has no room.
pub fn zero(f: &File, off: u64, len: u64) -> io::Result<()> {
    if len == 0 {
        return Ok(());
    }
    let off = i64::try_from(off).map_err(io::Error::other)?;
    let len = i64::try_from(len).map_err(io::Error::other)?;
    // SAFETY: the descriptor stays open for the whole call; non-negative offset and length are valid arguments
    let rc = unsafe { fallocate(f.as_raw_fd(), FALLOC_FL_ZERO_RANGE, off, len) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Linux `FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE`.
const FALLOC_FL_PUNCH_HOLE_KEEP_SIZE: i32 = 0x02 | 0x01;

/// Gives `[off, off + len)` of `f` back to the filesystem, the file size kept (a reservation handing its space to a
/// range the run takes next, workspace#526).
pub fn release(f: &File, off: u64, len: u64) -> io::Result<()> {
    if len == 0 {
        return Ok(());
    }
    let off = i64::try_from(off).map_err(io::Error::other)?;
    let len = i64::try_from(len).map_err(io::Error::other)?;
    // SAFETY: the descriptor stays open for the whole call; non-negative offset and length are valid arguments
    let rc = unsafe { fallocate(f.as_raw_fd(), FALLOC_FL_PUNCH_HOLE_KEEP_SIZE, off, len) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
