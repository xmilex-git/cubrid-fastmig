//! Opens a source volume read-only and holds a whole-file POSIX read lock on it for the rest of the run, so a CUBRID
//! server cannot boot on the source while the tool reads it, and the tool refuses a source a server holds.
//!
//! Input: the volume path, and whether to ask for direct I/O. Output: the open file; the lock lives as long as the
//! process keeps this file open. POSIX locks belong to the process and are dropped when *any* descriptor of the same
//! file in this process is closed, so every later read of a locked volume must go through the returned handle.
//! Source: engine `src/storage/file_io.c:fileio_lock` (`lockf (vol_fd, F_TLOCK, 0)`, a whole-file write lock that
//! fails on a conflicting lock), release/11.4 and develop; decision G1 workspace#481 (Q5).

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

/// Linux x86-64 `O_DIRECT`.
pub const O_DIRECT: i32 = 0o40000;
const F_SETLK: i32 = 6;
const F_RDLCK: i16 = 0;
const SEEK_SET: i16 = 0;

/// Linux x86-64 `struct flock`.
#[repr(C)]
struct Flock {
    l_type: i16,
    l_whence: i16,
    l_start: i64,
    l_len: i64,
    l_pid: i32,
}

unsafe extern "C" {
    fn fcntl(fd: i32, cmd: i32, ...) -> i32;
}

/// Opens `path` read-only (with `O_DIRECT` when `direct` and the filesystem takes it) and read-locks the whole file.
pub fn open_locked(path: &Path, direct: bool) -> io::Result<File> {
    let file = open_read_only(path, direct)?;
    let fl = Flock {
        l_type: F_RDLCK,
        l_whence: SEEK_SET,
        l_start: 0,
        l_len: 0,
        l_pid: 0,
    };
    // SAFETY: F_SETLK reads one `struct flock` through the pointer, which outlives the call.
    let rc = unsafe { fcntl(file.as_raw_fd(), F_SETLK, &fl as *const Flock) };
    if rc != 0 {
        let err = io::Error::last_os_error();
        return Err(io::Error::new(
            err.kind(),
            format!(
                "{}: cannot read-lock ({err}); is a server running on the source?",
                path.display()
            ),
        ));
    }
    Ok(file)
}

fn open_read_only(path: &Path, direct: bool) -> io::Result<File> {
    if direct {
        match OpenOptions::new()
            .read(true)
            .custom_flags(O_DIRECT)
            .open(path)
        {
            Ok(f) => return Ok(f),
            Err(e) if e.raw_os_error() == Some(22) => {} // EINVAL: no direct I/O on this filesystem
            Err(e) => return Err(e),
        }
    }
    File::open(path)
}
