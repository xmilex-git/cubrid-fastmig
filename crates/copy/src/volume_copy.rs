//! Moves every source volume to its target path: clone when the filesystem allows it, otherwise a parallel byte
//! copy in fixed-size chunks with the page treatment applied inline. All work shares the worker limit.
//!
//! Input: one job per volume (its read-only source handle and target path), the worker limit, the page treatment, and whether
//! clone is allowed. Output: per volume, the copy method used and the bytes moved. Target files are created new
//! (an existing file is an error) and recorded in the incomplete marker before they are written; they are
//! made durable later, with every other created file, by [`crate::durable`].
//! Progress: `meter` (the volume-copy phase) gets the bytes of each clone and of each 8 MiB copy task.
//! Source: decision G1 workspace#481 (clone whenever possible, copy otherwise; `--workers` slides over the work).

use std::fs::{File, OpenOptions};
use std::io;
use std::path::PathBuf;
use std::sync::Mutex;

use fastmig_ondisk::preallocate;
use fastmig_progress::Meter;

use crate::byte_copy::{CopyRange, InlineTreatment, copy_range};
use crate::clone_file::try_clone;
use crate::incomplete_marker::IncompleteMarker;
use crate::worker_pool;

/// Bytes one copy task moves (the unit the worker_limit slide over).
const CHUNK_BYTES: u64 = 8 << 20;
/// Each worker's I/O buffer, the copy engine's only per-worker memory. Small enough to stay in the CPU cache
/// between the read and the write: copying 1 GiB took 0.35 s with 256 KiB blocks and 0.99 s with 8 MiB blocks on
/// the dev host (dd, page cache warm, workspace#494). A multiple of every IO page size (4..64 KiB).
pub const IO_BUF_BYTES: usize = 256 << 10;

pub struct VolumeJob {
    pub volid: i16,
    /// Opened read-only by [`crate::source_lock::open_source`].
    pub source: File,
    pub target: PathBuf,
    pub io_pagesize: usize,
    /// The target was created and allocated in full by [`preallocate_targets`] at the start of the run: it is
    /// byte-copied into, never cloned (workspace#526).
    pub preallocated: bool,
    /// The target was cloned by [`clone_targets`] at the start of the run: kept as is (workspace#526).
    pub precloned: bool,
}

pub use fastmig_plan::CopyMethod;

#[derive(Debug)]
pub struct VolumeOutcome {
    pub volid: i16,
    pub method: CopyMethod,
    pub bytes: u64,
}

/// Copies all `jobs` with at most `workers` threads. With `allow_clone` false every volume is byte-copied (used to
/// measure the byte copy on a filesystem that could clone). `treatment` runs only on byte-copied volumes; a cloned volume is
/// treated in place by the caller.
pub fn copy_volumes(
    jobs: &[VolumeJob],
    worker_limit: usize,
    treatment: &dyn InlineTreatment,
    allow_clone: bool,
    marker: &IncompleteMarker,
    meter: Meter,
) -> io::Result<Vec<VolumeOutcome>> {
    // Pass 1: create every target and try to clone it; a volume that cannot be cloned is split into copy chunks.
    let prepared: Vec<Mutex<Option<(File, CopyMethod, u64)>>> =
        jobs.iter().map(|_| Mutex::new(None)).collect();
    worker_pool::run(jobs.len(), worker_limit, |i| {
        let job = &jobs[i];
        let size = job.source.metadata()?.len();
        if !size.is_multiple_of(job.io_pagesize as u64) {
            return Err(io::Error::other(format!(
                "volume {} size {size} is not a multiple of the IO page size {}",
                job.volid, job.io_pagesize
            )));
        }
        if job.preallocated || job.precloned {
            let dst = OpenOptions::new().write(true).open(&job.target)?;
            let method = if job.precloned {
                meter.add(size);
                CopyMethod::Clone
            } else {
                CopyMethod::Copy
            };
            *prepared[i].lock().unwrap() = Some((dst, method, size));
            return Ok(());
        }
        marker.record(&job.target)?;
        let dst = create_target(&job.target)?;
        let method = if allow_clone && try_clone(&job.source, &dst)? {
            CopyMethod::Clone
        } else {
            dst.set_len(size)?;
            CopyMethod::Copy
        };
        if method == CopyMethod::Clone {
            meter.add(size);
        }
        *prepared[i].lock().unwrap() = Some((dst, method, size));
        Ok(())
    })?;
    let prepared: Vec<(File, CopyMethod, u64)> = prepared
        .into_iter()
        .map(|m| m.into_inner().unwrap().expect("pass 1 filled every slot"))
        .collect();

    // Pass 2: chunks of all byte-copied volumes, in volume order, on one pool.
    let mut chunks = Vec::new();
    for (i, (_, method, size)) in prepared.iter().enumerate() {
        if *method == CopyMethod::Copy {
            let mut start = 0;
            while start < *size {
                let end = (start + CHUNK_BYTES).min(*size);
                chunks.push((i, start, end));
                start = end;
            }
        }
    }
    let bufs: Vec<Mutex<Vec<u8>>> = (0..worker_limit.clamp(1, chunks.len().max(1)))
        .map(|_| Mutex::new(vec![0u8; IO_BUF_BYTES]))
        .collect();
    worker_pool::run(chunks.len(), worker_limit, |c| {
        let (i, start, end) = chunks[c];
        let range = CopyRange {
            src: &jobs[i].source,
            dst: &prepared[i].0,
            volid: jobs[i].volid,
            io_pagesize: jobs[i].io_pagesize,
            start,
            end,
        };
        // a free buffer always exists: there are as many buffers as threads
        let mut buf = bufs
            .iter()
            .find_map(|b| b.try_lock().ok())
            .expect("one buffer per worker");
        copy_range(&range, &mut buf, treatment)?;
        meter.add(end - start);
        Ok(())
    })?;

    Ok(jobs
        .iter()
        .zip(&prepared)
        .map(|(job, (_, method, size))| VolumeOutcome {
            volid: job.volid,
            method: *method,
            bytes: *size,
        })
        .collect())
}

/// Creates the target of every job marked `preallocated` and allocates its full size, before the run writes
/// anything else (workspace#526: the space a byte copy needs is taken at the start, so another process cannot take
/// it during the run). Recorded in the marker first, like every target.
pub fn preallocate_targets(jobs: &[VolumeJob], marker: &IncompleteMarker) -> io::Result<()> {
    for job in jobs.iter().filter(|j| j.preallocated) {
        let size = job.source.metadata()?.len();
        marker.record(&job.target)?;
        let dst = create_target(&job.target)?;
        preallocate::allocate(&dst, size).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!("allocating {size} B for {}: {e}", job.target.display()),
            )
        })?;
    }
    Ok(())
}

/// Clones the target of every job not byte-copied, at the start of the run (workspace#526: the pages the run rewrites
/// in a clone are unshared right after, before the reservation is taken). A volume the filesystem refuses to clone
/// stops the run: the free-space check counted it as a clone.
pub fn clone_targets(jobs: &mut [VolumeJob], marker: &IncompleteMarker) -> io::Result<()> {
    for job in jobs.iter_mut().filter(|j| !j.preallocated) {
        marker.record(&job.target)?;
        let dst = create_target(&job.target)?;
        if !try_clone(&job.source, &dst)? {
            return Err(io::Error::other(format!(
                "{}: the filesystem refused to clone volume {} (the plan counted it as a clone); run with --no-clone",
                job.target.display(),
                job.volid
            )));
        }
        job.precloned = true;
    }
    Ok(())
}

/// Creates the target new (an existing file is an error).
fn create_target(path: &PathBuf) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
}
