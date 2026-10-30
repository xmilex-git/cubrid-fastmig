//! Free bytes of the filesystem a target directory will live on, for the pre-check on target space.
//!
//! Input: a path (it may not exist yet: its nearest existing ancestor is asked). Output: bytes available to an
//! unprivileged writer (`f_bavail * f_frsize`).
//! Source: POSIX `statvfs(3)`, glibc x86_64 `struct statvfs` layout (eleven 8-byte fields, then 6 ints).

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

#[repr(C)]
struct StatVfs {
    f_bsize: u64,
    f_frsize: u64,
    f_blocks: u64,
    f_bfree: u64,
    f_bavail: u64,
    f_files: u64,
    f_ffree: u64,
    f_favail: u64,
    f_fsid: u64,
    f_flag: u64,
    f_namemax: u64,
    spare: [i32; 6],
}

unsafe extern "C" {
    fn statvfs(path: *const std::ffi::c_char, buf: *mut StatVfs) -> i32;
}

pub fn available_bytes(p: &Path) -> io::Result<u64> {
    let mut at = p;
    while !at.exists() {
        at = at
            .parent()
            .ok_or_else(|| io::Error::other(format!("{}: no existing ancestor", p.display())))?;
    }
    let c = CString::new(at.as_os_str().as_bytes()).map_err(io::Error::other)?;
    let mut s = std::mem::MaybeUninit::<StatVfs>::uninit();
    // SAFETY: `c` is a NUL-terminated path and `s` is a buffer of the struct statvfs(3) fills on success.
    let rc = unsafe { statvfs(c.as_ptr(), s.as_mut_ptr()) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: statvfs returned 0, so it wrote the whole struct.
    let s = unsafe { s.assume_init() };
    Ok(s.f_bavail.saturating_mul(s.f_frsize))
}

/// `XFS_SUPER_MAGIC` (statfs `f_type`).
const XFS_SUPER_MAGIC: i64 = 0x5846_5342;

unsafe extern "C" {
    fn statfs(path: *const std::ffi::c_char, buf: *mut [i64; 16]) -> i32;
}

/// Whether the filesystem of `p` (or its nearest existing ancestor) can unshare a clone's blocks in place
/// (`FALLOC_FL_UNSHARE_RANGE`): XFS (workspace#526; other filesystems keep the rewritten pages in the reservation).
pub fn can_unshare(p: &Path) -> io::Result<bool> {
    let mut at = p;
    while !at.exists() {
        at = at
            .parent()
            .ok_or_else(|| io::Error::other(format!("{}: no existing ancestor", p.display())))?;
    }
    let c = CString::new(at.as_os_str().as_bytes()).map_err(io::Error::other)?;
    // x86-64 `struct statfs` is 15 longs: f_type first
    let mut s = [0i64; 16];
    // SAFETY: `c` is a NUL-terminated path; `s` is larger than the struct statfs(2) fills on success.
    if unsafe { statfs(c.as_ptr(), &mut s) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(s[0] == XFS_SUPER_MAGIC)
}
