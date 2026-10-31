//! Allocates a file's space on its filesystem up front, so later writes into it cannot fail for want of space.
//!
//! Input: an open file and a length. Output: the blocks of `[0, len)` allocated (the file grows to `len`).
//! Used for the spill arena, the byte-copied target volumes and the new log, which a run takes before its first write;
//! [`unallocated_pages`] for the pages of a cloned volume whose content the run does not need (workspace#526: no disk
//! full during a run; the rewritten pages are written over at the start instead, workspace#556; formatted as
//! unallocated pages, workspace#591).
//! Source: POSIX `posix_fallocate(3)` (glibc on XFS/ext4: real allocation, not a sparse extension); Linux
//! `fallocate(2)` (`FALLOC_FL_ZERO_RANGE`, `FALLOC_FL_PUNCH_HOLE`). Unallocated page bytes: engine develop @682b080b2
//! `src/storage/file_io.c:11714` (`fileio_initialize_res`: NULL LSA, VPID -1|-1), `:2503` (`fileio_expand_to`),
//! `:4019-4046` (`fileio_write_or_add_to_dwb`: the own VPID on a mounted volume), `src/storage/page_buffer.c:5416-5444`
//! (`pgbuf_set_bcb_page_vpid`: header -1|-1 or the page's own VPID, else an assert); same lines at @5b1867174 but
//! page_buffer.c 5391-5419 (workspace#591, #593).

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
/// page gets a NULL LSA in its header and watermark and its own VPID: the bytes the engine's volume expansion writes
/// on a mounted volume (`fileio_expand_to`: `fileio_initialize_res` leaves VPID -1|-1, then `fileio_write_or_add_to_dwb`
/// writes the page's own VPID into it), as the source's unallocated pages hold; a volume formatted before it is
/// mounted keeps -1|-1 ([`crate::volume_set::unwritten_page`]), and the engine takes both. A zero header is VPID
/// (0, 0), which the engine's first fix of the page asserts on (`pgbuf_set_bcb_page_vpid`, workspace#591).
/// `off` and `len` are whole pages.
pub fn unallocated_pages(
    f: &File,
    volid: i16,
    io_pagesize: u64,
    off: u64,
    len: u64,
) -> io::Result<()> {
    debug_assert_eq!(off % io_pagesize, 0, "unallocated_pages: off not on a page");
    debug_assert_eq!(
        len % io_pagesize,
        0,
        "unallocated_pages: len not whole pages"
    );
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
