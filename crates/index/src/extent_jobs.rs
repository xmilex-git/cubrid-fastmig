//! Runs one job per extent on `--workers` threads, the extents read in one of two ways: each worker reads its own
//! (parallel reads), or one reader thread reads them all in volume order and hands each over (one read stream).
//!
//! Input: the volumes, extents in physical order (`page_extents::extents`), the worker count, the way to read, and
//! the job, which gets the extent and its bytes or the read error. Output: each job's result, in extent order.
//! Why two ways: on SSD and NVMe parallel reads are the fastest; on a rotating disk the workers' reads of
//! neighbouring extents still seek against each other (2 GB T31 source, W16: about 120 MB/s against 231 MB/s for
//! one stream), so there one thread reads forward and the workers only extract (workspace#596).
//! Memory: parallel, one extent buffer per worker; one stream, `workers + 2` buffers passed around.

use std::io;
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

use fastmig_ondisk::page_extents::Extent;
use fastmig_ondisk::volume_set::VolumeSet;

use crate::job_pool::run_jobs;

/// How the extents are read; chosen once per run from the source's storage kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtentReads {
    /// Each worker reads the extent it takes (SSD, NVMe).
    Parallel,
    /// One reader thread reads every extent in order (a rotating disk).
    OneStream,
}

/// Extent buffers in flight in [`ExtentReads::OneStream`] beyond one per worker: the reader fills the next ones
/// while the workers extract.
pub const READ_AHEAD: usize = 2;

pub fn extent_jobs<T: Send + Sync, R: Send>(
    vols: &VolumeSet,
    workers: usize,
    reads: ExtentReads,
    extents: Vec<Extent<T>>,
    job: impl Fn(&Extent<T>, io::Result<&[u8]>) -> R + Sync,
) -> Vec<R> {
    match reads {
        ExtentReads::Parallel => run_jobs(workers, extents, |e| e.read(vols, |b| job(&e, b))),
        ExtentReads::OneStream => one_stream(vols, workers, extents, job),
    }
}

/// A read extent on its way to a worker: its position, and its bytes or the read error.
type Filled = (usize, io::Result<Vec<u8>>);

fn one_stream<T: Sync, R: Send>(
    vols: &VolumeSet,
    workers: usize,
    extents: Vec<Extent<T>>,
    job: impl Fn(&Extent<T>, io::Result<&[u8]>) -> R + Sync,
) -> Vec<R> {
    let workers = workers.max(1);
    let (free_tx, free_rx): (SyncSender<Vec<u8>>, Receiver<Vec<u8>>) =
        sync_channel(workers + READ_AHEAD);
    for _ in 0..workers + READ_AHEAD {
        free_tx.send(Vec::new()).unwrap();
    }
    let (full_tx, full_rx) = sync_channel::<Filled>(workers + READ_AHEAD);
    let full_rx = Mutex::new(full_rx);
    let results: Vec<Mutex<Option<R>>> = extents.iter().map(|_| Mutex::new(None)).collect();
    let io_pagesize = vols.io_pagesize();
    let ext = &extents;
    std::thread::scope(|s| {
        // the reader owns the sending side, so the queue closes when it is done
        s.spawn(move || {
            for (i, e) in ext.iter().enumerate() {
                // a buffer comes back from a worker once its job is done
                let Ok(mut buf) = free_rx.recv() else { return };
                buf.resize(e.npages * io_pagesize, 0);
                let read = vols.read_pages_into(e.first, &mut buf).map(|()| buf);
                if full_tx.send((i, read)).is_err() {
                    return;
                }
            }
        });
        for _ in 0..workers {
            s.spawn(|| {
                loop {
                    let next = full_rx.lock().unwrap().recv();
                    let Ok((i, read)) = next else { break };
                    let (r, buf) = match read {
                        Ok(buf) => (job(&extents[i], Ok(&buf)), buf),
                        Err(err) => (job(&extents[i], Err(err)), Vec::new()),
                    };
                    *results[i].lock().unwrap() = Some(r);
                    let _ = free_tx.send(buf);
                }
            });
        }
    });
    results
        .into_iter()
        .map(|r| r.into_inner().unwrap().unwrap())
        .collect()
}
