//! Moves every source volume to its target path: clone when the filesystem allows it, otherwise a parallel byte
//! copy in fixed-size chunks with the page hook applied inline. All work shares one thread cap.
//!
//! Input: one job per volume (its locked source handle and target path), the worker cap, the hook, and whether
//! clone is allowed. Output: per volume, the copy method used and the bytes moved. Target files are created new
//! (an existing file is an error) and recorded in the incomplete marker before they are written; they are
//! made durable later, with every other created file, by [`crate::durable`].
//! Source: decision G1 workspace#481 (clone whenever possible, copy otherwise; `--workers` slides over the work).

use std::fs::{File, OpenOptions};
use std::io;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::byte_copy::{CopyRange, PageHook, copy_range};
use crate::clone_file::try_clone;
use crate::incomplete_marker::IncompleteMarker;
use crate::worker_pool;

/// Bytes one copy task moves; also each worker's buffer size.
const CHUNK_BYTES: u64 = 8 << 20;

pub struct VolumeJob {
    pub volid: i16,
    /// Opened read-only by [`crate::source_lock::open_source`].
    pub source: File,
    pub target: PathBuf,
    pub io_pagesize: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyMethod {
    Clone,
    Copy,
}

#[derive(Debug)]
pub struct VolumeOutcome {
    pub volid: i16,
    pub method: CopyMethod,
    pub bytes: u64,
}

/// Copies all `jobs` with at most `workers` threads. With `allow_clone` false every volume is byte-copied (used to
/// measure copy mode on a filesystem that could clone). `hook` runs only on byte-copied volumes; a cloned volume is
/// treated in place by the caller.
pub fn copy_volumes(
    jobs: &[VolumeJob],
    workers: usize,
    hook: &dyn PageHook,
    allow_clone: bool,
    marker: &IncompleteMarker,
) -> io::Result<Vec<VolumeOutcome>> {
    // Pass 1: create every target and try to clone it; a volume that cannot be cloned is split into copy chunks.
    let prepared: Vec<Mutex<Option<(File, CopyMethod, u64)>>> =
        jobs.iter().map(|_| Mutex::new(None)).collect();
    worker_pool::run(jobs.len(), workers, |i| {
        let job = &jobs[i];
        let size = job.source.metadata()?.len();
        if !size.is_multiple_of(job.io_pagesize as u64) {
            return Err(io::Error::other(format!(
                "volume {} size {size} is not a multiple of the IO page size {}",
                job.volid, job.io_pagesize
            )));
        }
        marker.record(&job.target)?;
        let dst = create_target(&job.target)?;
        let method = if allow_clone && try_clone(&job.source, &dst)? {
            CopyMethod::Clone
        } else {
            dst.set_len(size)?;
            CopyMethod::Copy
        };
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
    let bufs: Vec<Mutex<Vec<u8>>> = (0..workers.clamp(1, chunks.len().max(1)))
        .map(|_| Mutex::new(vec![0u8; CHUNK_BYTES as usize]))
        .collect();
    worker_pool::run(chunks.len(), workers, |c| {
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
        copy_range(&range, &mut buf, hook)
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

/// Creates the target new (an existing file is an error).
fn create_target(path: &PathBuf) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
}
