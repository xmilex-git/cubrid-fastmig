//! Opens source files read-only, and holds a whole-file POSIX read lock on the source active log `<db>_lgat` for
//! the rest of the run: a CUBRID server cannot boot on the source while the tool reads it, and the tool refuses a
//! source whose server is up.
//!
//! The lock goes on the active log, not on the volumes: the engine write-locks only the active log at boot
//! (`log_initialize_internal` mounts it with `lock_wait` true) and mounts data volumes without a lock
//! (`boot_mount` passes `lock_wait` false), so a read lock on a volume does not stop a server (checked on develop
//! release, workspace#494).
//! Input: a path. Output: the open file; a lock lives as long as the process keeps that file open. POSIX locks
//! belong to the process and are dropped when *any* descriptor of the same file in this process is closed, so
//! every later read of the locked log must go through the returned handle.
//! Source: engine `src/storage/file_io.c:fileio_lock` (`fileio_lock_file_write` = `F_SETLK F_WRLCK` on the whole
//! file, fails on a conflicting lock), `log_manager.c:log_initialize_internal`, `boot_sr.c:boot_mount`
//! (release/11.4 and develop @5b1867174); decision G1 workspace#481 (Q5).

use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::path::Path;

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

/// Opens the source active log read-only and read-locks it; fails when a server holds it.
pub fn open_locked_log(path: &Path) -> io::Result<File> {
    let file = open_source(path)?;
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

/// Opens a source file read-only.
pub fn open_source(path: &Path) -> io::Result<File> {
    File::open(path).map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))
}
