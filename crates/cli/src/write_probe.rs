//! Write probe: the sustained write throughput of the target directory's device, measured at the start of a migrate.
//! The byte phases that write the target (the clone ranges, the fsync) take their default rates from it instead of
//! from the storage kind, so a device whose kind is unknown, or faster or slower than its kind, is estimated by what
//! it does (workspace#603; the storage kind still picks the precheck's read order and is shown in the plan).
//!
//! Input: the target directory (it need not exist yet: its nearest existing ancestor is written). Output:
//! [`WriteProbe`], or `None` when the probe cannot run (no `O_TMPFILE` or `O_DIRECT` on that filesystem, no space,
//! any I/O error): the caller then keeps the storage kind's defaults; a failed probe never stops the run.
//! Method: 1 MiB blocks written in order with `O_DIRECT` (no page cache) into an `O_TMPFILE` file (unnamed: it
//! disappears when closed, also when the process is killed, so nothing is left to remove), until [`PROBE_S`] has passed
//! or [`PROBE_BYTES`] are written, then one `fdatasync`; the rate is the bytes over the whole time, the fdatasync
//! included. A drive's write cache takes the first blocks at the interface's speed, but the fdatasync waits until
//! the cache has reached the medium, so the rate is the medium's (an HDD with a 256 MiB cache measures its own
//! sequential write, workspace#603).
//! Source: Linux `open(2)` (`O_TMPFILE`, `O_DIRECT`: x86_64 values below), `fdatasync(2)`.

use std::fs::{File, OpenOptions};
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::Path;
use std::time::Instant;

/// `__O_TMPFILE | O_DIRECTORY` on x86_64 Linux.
const O_TMPFILE: i32 = 0o20200000;
/// `O_DIRECT` on x86_64 Linux.
const O_DIRECT: i32 = 0o40000;
/// One write; a multiple of every logical block size `O_DIRECT` asks the buffer and offset to be aligned to.
const BLOCK: usize = 1 << 20;
const ALIGN: usize = 4096;
/// Seconds of writing before the fdatasync: past a 256 MiB drive cache on the dev host HDD.
pub const PROBE_S: f64 = 1.0;
/// Bytes written at most: a fast device stops here, well under a second.
pub const PROBE_BYTES: u64 = 512 << 20;

/// What the probe wrote and how long it took, the fdatasync included.
#[derive(Clone, Copy, Debug)]
pub struct WriteProbe {
    pub bytes: u64,
    pub seconds: f64,
}

impl WriteProbe {
    /// Bytes per second.
    pub fn rate(&self) -> f64 {
        self.bytes as f64 / self.seconds.max(1e-6)
    }
}

/// Probes the device of `dir`, or `None` when it cannot (see the module doc).
pub fn measure(dir: &Path) -> Option<WriteProbe> {
    let dir = dir.ancestors().find(|a| a.is_dir())?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(O_TMPFILE | O_DIRECT)
        .open(dir)
        .ok()?;
    write_until_done(&file)
}

fn write_until_done(file: &File) -> Option<WriteProbe> {
    // an aligned 1 MiB window into a larger buffer; not zeros, so no device can skip writing them
    let mut buf = vec![0u8; BLOCK + ALIGN];
    let start = buf.as_ptr().align_offset(ALIGN);
    let block = &mut buf[start..start + BLOCK];
    let mut x: u32 = 0x9e37_79b9;
    for b in block.iter_mut() {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        *b = x as u8;
    }
    let t = Instant::now();
    let mut bytes = 0u64;
    while bytes < PROBE_BYTES && t.elapsed().as_secs_f64() < PROBE_S {
        file.write_all_at(block, bytes).ok()?;
        bytes += BLOCK as u64;
    }
    file.sync_data().ok()?;
    Some(WriteProbe {
        bytes,
        seconds: t.elapsed().as_secs_f64(),
    })
}
