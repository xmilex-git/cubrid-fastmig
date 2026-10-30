//! The bytes of a file's blocks no other file shares: what deleting it gives back to the filesystem. A reflink
//! clone shares most of its blocks with its source, so its `st_blocks` overstates that (workspace#526: the free
//! space a rerun gets from removing a killed run's files).
//!
//! Input: an open file. Output: the length of its extents without `FIEMAP_EXTENT_SHARED`; the allocated size
//! (`st_blocks`) where the filesystem has no FIEMAP (it shares nothing then).
//! Source: Linux `Documentation/filesystems/fiemap.rst` (`FS_IOC_FIEMAP`, `struct fiemap`, `struct
//! fiemap_extent`, x86-64 layout).

use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;

/// `_IOWR('f', 11, struct fiemap)`.
const FS_IOC_FIEMAP: u64 = 0xC020_660B;
const FIEMAP_FLAG_SYNC: u32 = 0x1;
const FIEMAP_EXTENT_LAST: u32 = 0x1;
const FIEMAP_EXTENT_SHARED: u32 = 0x2000;
/// Extents asked for per call.
const BATCH: usize = 256;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Extent {
    logical: u64,
    physical: u64,
    length: u64,
    reserved64: [u64; 2],
    flags: u32,
    reserved: [u32; 3],
}

#[repr(C)]
struct Fiemap {
    start: u64,
    length: u64,
    flags: u32,
    mapped_extents: u32,
    extent_count: u32,
    reserved: u32,
    extents: [Extent; BATCH],
}

unsafe extern "C" {
    fn ioctl(fd: i32, request: u64, ...) -> i32;
}

pub fn own_bytes(f: &File) -> io::Result<u64> {
    let mut own = 0u64;
    let mut start = 0u64;
    loop {
        let mut m = Box::new(Fiemap {
            start,
            length: u64::MAX - start,
            flags: FIEMAP_FLAG_SYNC,
            mapped_extents: 0,
            extent_count: BATCH as u32,
            reserved: 0,
            extents: [Extent::default(); BATCH],
        });
        // SAFETY: FS_IOC_FIEMAP reads the header and fills at most `extent_count` extents of the buffer, which
        // lives for the call.
        if unsafe { ioctl(f.as_raw_fd(), FS_IOC_FIEMAP, &mut *m as *mut Fiemap) } != 0 {
            let e = io::Error::last_os_error();
            return match e.raw_os_error() {
                // no FIEMAP: nothing is shared there
                Some(95) | Some(25) => Ok(f.metadata()?.blocks() * 512),
                _ => Err(e),
            };
        }
        let n = m.mapped_extents as usize;
        if n == 0 {
            return Ok(own);
        }
        for e in &m.extents[..n] {
            if e.flags & FIEMAP_EXTENT_SHARED == 0 {
                own += e.length;
            }
        }
        let last = m.extents[n - 1];
        if last.flags & FIEMAP_EXTENT_LAST != 0 {
            return Ok(own);
        }
        start = last.logical + last.length;
    }
}
