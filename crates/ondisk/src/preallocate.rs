//! Allocates a file's space on its filesystem up front, so later writes into it cannot fail for want of space.
//!
//! Input: an open file and a length. Output: the blocks of `[0, len)` allocated (the file grows to `len`).
//! Used for the spill arena, the byte-copied target volumes and the new log, which a run takes before its first write;
//! [`unallocated_pages`] for the pages of a cloned volume whose content the run does not need (workspace#526: no disk
//! full during a run; the rewritten pages are written over at the start instead, workspace#556; formatted as
//! unallocated pages, workspace#591).
//! Source: POSIX `posix_fallocate(3)` (glibc on XFS/ext4: real allocation, not a sparse extension).

use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::FileExt;

use crate::ids::Vpid;
use crate::page::{self, IO_RESERVED_SIZE, IO_TRAILER_SIZE, PageType};

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

/// Gives the pages `[off, off + len)` of volume `volid` in `f` blocks of their own without copying what they held,
/// as unallocated pages: for pages whose content the run does not need — free sectors it will take, a heap's
/// unallocated pages — where a write of the source bytes would copy them (workspace#526, #556). The range is zeroed
/// unwritten (`FALLOC_FL_ZERO_RANGE`, fails before changing anything when the filesystem has no room), then every
/// page gets the header and watermark the engine formats a page with: NULL LSA and the page's own VPID
/// (`fileio_initialize_res`, `fileio_write_or_add_to_dwb`), as the source's unallocated pages hold. A zero header is
/// VPID (0, 0), which the engine's first fix of the page asserts on (`pgbuf_set_bcb_page_vpid`, workspace#591).
pub fn unallocated_pages(
    f: &File,
    volid: i16,
    io_pagesize: u64,
    off: u64,
    len: u64,
) -> io::Result<()> {
    if len == 0 {
        return Ok(());
    }
    let (o, l) = (
        i64::try_from(off).map_err(io::Error::other)?,
        i64::try_from(len).map_err(io::Error::other)?,
    );
    // SAFETY: the descriptor stays open for the whole call; non-negative offset and length are valid arguments
    if unsafe { fallocate(f.as_raw_fd(), FALLOC_FL_ZERO_RANGE, o, l) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let n = io_pagesize as usize;
    let mut page = vec![0u8; n];
    for at in (off..off + len).step_by(n) {
        let pageid = i32::try_from(at / io_pagesize).map_err(io::Error::other)?;
        page::init_header(&mut page, Vpid::new(volid, pageid), PageType::Unknown);
        f.write_all_at(&page[..IO_RESERVED_SIZE], at)?;
        f.write_all_at(
            &page[n - IO_TRAILER_SIZE..],
            at + io_pagesize - IO_TRAILER_SIZE as u64,
        )?;
    }
    Ok(())
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
