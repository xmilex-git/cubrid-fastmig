//! Write probe: the sustained write throughput of the target directory's device, measured at the start of a migrate
//! when the target's storage kind is unknown. The byte phases that write the target (the clone ranges, the fsync) then
//! take their default rates from it instead of the hdd defaults, so an unknown device is estimated by what it does
//! (workspace#603). A known kind keeps its own defaults and is not probed: one probe stream underrated the SATA SSD's
//! writeback, and no one factor fit both the HDD and the SATA SSD (workspace#606 plan B).
//!
//! Input: the target directory (it need not exist yet: its nearest existing ancestor is written). Output:
//! [`WriteProbe`], or `None` when the probe cannot run (no `O_TMPFILE` or `O_DIRECT` on that filesystem, no space,
//! any I/O error): the caller then keeps the storage kind's defaults; a failed probe never stops the run.
//! Method: two passes into one `O_TMPFILE` file (unnamed: it disappears when closed, also when the process is killed,
//! so nothing is left to remove) with `O_DIRECT` (no page cache), each writing 1 MiB blocks until [`PROBE_S`] has
//! passed or [`PROBE_BYTES`] are written, then one `fdatasync`; a pass's rate is its bytes over its whole time, the
//! fdatasync included.
//! - sequential: blocks in order from offset 0. A drive's write cache takes the first blocks at the interface's speed,
//!   but the fdatasync waits until the cache has reached the medium, so the rate is the medium's (an HDD with a 256 MiB
//!   cache measures its own sequential write, workspace#603). The fsync's default follows it.
//! - scattered: blocks at pseudo-random distinct places of a [`SPAN`] range allocated by `fallocate` after the first
//!   pass's bytes (allocated first, so the places on the file are places on the device). The clone ranges are written
//!   scattered over the volumes and synced at 0.61-0.74 of the sequential rate on the HDD, 0.98-1.11 on the SATA SSD:
//!   the HDD seeks between them (fio random / sequential 1 MiB writes over 4 GiB: 0.76 and 1.00, workspace#608). The
//!   clone ranges' default follows this pass; it is skipped, and they follow the sequential pass as before, when the
//!   span is over [`FREE_SHARE`] of the filesystem's free space or `fallocate` fails (no support).
//!
//! Source: Linux `open(2)` (`O_TMPFILE`, `O_DIRECT`: x86_64 values below), `fallocate(2)`, `fdatasync(2)`; POSIX
//! `statvfs(3)`.

use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::Path;
use std::time::Instant;

use fastmig_plan::free_space;

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
/// The scattered pass's range, a power of two of blocks. Its scattered / sequential rate on the dev host (3 probes per
/// span, workspace#608) by span 1 / 4 / 16 / 64 GiB: HDD 0.80 / 0.82 / 0.77 / 0.72, SATA SSD 0.98 / 0.99 / 0.99 / 0.97.
/// Up to 4 GiB the HDD's blocks stay close enough to seek little; from 16 GiB its ratio moves toward the clone
/// ranges' (0.67 of the sequential probe, against 0.94 on the SATA SSD), so one factor fits both devices within 5%
/// ([`CLONED_RANGE_OF_SCATTERED_PROBE`](fastmig_progress::default_rates::CLONED_RANGE_OF_SCATTERED_PROBE)); 64 GiB
/// fits within 2% but holds four times the space for the same second.
pub const SPAN: u64 = 16 << 30;
/// The span's largest share of the target filesystem's free space: the span holds that space for about a second, on a
/// disk other processes may be writing to; under this share a nearly full disk keeps 90% of what was free, and a
/// disk with less than ten spans free is not probed scattered (workspace#608, 1번 세션 2026-11-02).
pub const FREE_SHARE: f64 = 0.1;

unsafe extern "C" {
    fn fallocate(fd: i32, mode: i32, offset: i64, len: i64) -> i32;
}

/// What one pass wrote and how long it took, the fdatasync included.
#[derive(Clone, Copy, Debug)]
pub struct Pass {
    pub bytes: u64,
    pub seconds: f64,
}

impl Pass {
    /// Bytes per second.
    pub fn rate(&self) -> f64 {
        self.bytes as f64 / self.seconds.max(1e-6)
    }
}

/// The two passes; `scattered` is `None` when that pass was skipped or failed.
#[derive(Clone, Copy, Debug)]
pub struct WriteProbe {
    pub sequential: Pass,
    pub scattered: Option<Pass>,
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
    let mut next = 0;
    let sequential = write_pass(&file, block, || {
        next += BLOCK as u64;
        next - BLOCK as u64
    })?;
    let room =
        free_space::available_bytes(dir).is_ok_and(|free| SPAN as f64 <= free as f64 * FREE_SHARE);
    // SAFETY: the descriptor stays open for the whole call; the offset and the length are non-negative
    let allocated =
        room && unsafe { fallocate(file.as_raw_fd(), 0, PROBE_BYTES as i64, SPAN as i64) } == 0;
    // the blocks of the range in a full-period order: an LCG modulo a power of two with c odd and a = 1 mod 4
    let mask = SPAN / BLOCK as u64 - 1;
    let mut slot = 0u64;
    let scattered = allocated
        .then(|| {
            write_pass(&file, block, || {
                slot = (slot
                    .wrapping_mul(0x5851_f42d_4c95_7f2d)
                    .wrapping_add(0x1405_7b7e_f767_814f))
                    & mask;
                PROBE_BYTES + slot * BLOCK as u64
            })
        })
        .flatten();
    Some(WriteProbe {
        sequential,
        scattered,
    })
}

/// Writes `block` at the offsets `at` gives, up to [`PROBE_BYTES`] or [`PROBE_S`], then fdatasyncs.
fn write_pass(file: &File, block: &[u8], mut at: impl FnMut() -> u64) -> Option<Pass> {
    let t = Instant::now();
    let mut bytes = 0u64;
    while bytes < PROBE_BYTES && t.elapsed().as_secs_f64() < PROBE_S {
        file.write_all_at(block, at()).ok()?;
        bytes += BLOCK as u64;
    }
    file.sync_data().ok()?;
    Some(Pass {
        bytes,
        seconds: t.elapsed().as_secs_f64(),
    })
}
