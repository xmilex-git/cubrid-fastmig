//! The pages a run rewrites in its cloned volumes, unshared in the background while the run goes on (workspace#526:
//! the start-time unshare no longer delays the first write). Every write to a target page passes the volume set's
//! write gate ([`UnshareGate::ensure`]): a page in a range not unshared yet waits for the background worker on it,
//! or unshares the range itself when no worker has started it, so the range's blocks are taken — or the write fails —
//! before the first byte of it is written.
//!
//! Input: the cloned target volumes' files and the ranges to unshare (`TargetSpace::unshare_ranges` but those holding
//! a volume's first page, which the run writes before the volume set opens). Output: the ranges unshared; how often a
//! writer took or waited for a range ([`GateStats`], report.json).
//! Source: workspace#526 (1번 세션: the wait sits at the one common write point, `VolumeSet::write_page`).

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use fastmig_ondisk::{Vpid, preallocate};
use fastmig_plan::migration_plan::PageRange;

const PENDING: u8 = 0;
const RUNNING: u8 = 1;
const DONE: u8 = 2;
const FAILED: u8 = 3;

struct VolumeRanges {
    volid: i16,
    file: File,
    /// Sorted by first page, disjoint.
    ranges: Vec<PageRange>,
    state: Vec<AtomicU8>,
}

pub struct UnshareGate {
    volumes: Vec<VolumeRanges>,
    io_pagesize: u64,
    lock: Mutex<()>,
    done: Condvar,
    taken: AtomicU64,
    waits: AtomicU64,
    wait_ns: AtomicU64,
    first_error: Mutex<Option<String>>,
}

/// How the writers met the background unshare.
#[derive(Clone, Copy, Debug, Default)]
pub struct GateStats {
    /// Ranges a writer reached before any worker and unshared itself.
    pub taken_by_writers: u64,
    /// Writes that waited for a worker's range, and the time they waited.
    pub waits: u64,
    pub wait_seconds: f64,
}

impl UnshareGate {
    /// `targets`: (volid, file) of every cloned target volume; `ranges` merged, as `TargetSpace` gives them.
    pub fn new(
        targets: &[(i16, &Path)],
        ranges: &[PageRange],
        io_pagesize: u64,
    ) -> io::Result<Self> {
        let mut volumes = Vec::new();
        for &(volid, path) in targets {
            let mut rs: Vec<PageRange> = ranges
                .iter()
                .filter(|r| r.volid == volid)
                .copied()
                .collect();
            if rs.is_empty() {
                continue;
            }
            rs.sort_by_key(|r| r.first);
            volumes.push(VolumeRanges {
                volid,
                file: OpenOptions::new().write(true).open(path)?,
                state: rs.iter().map(|_| AtomicU8::new(PENDING)).collect(),
                ranges: rs,
            });
        }
        Ok(UnshareGate {
            volumes,
            io_pagesize,
            lock: Mutex::new(()),
            done: Condvar::new(),
            taken: AtomicU64::new(0),
            waits: AtomicU64::new(0),
            wait_ns: AtomicU64::new(0),
            first_error: Mutex::new(None),
        })
    }

    fn find(&self, vpid: Vpid) -> Option<(&VolumeRanges, usize)> {
        let v = self.volumes.iter().find(|v| v.volid == vpid.volid)?;
        let i = v
            .ranges
            .partition_point(|r| r.first as i64 + r.pages as i64 <= vpid.pageid as i64);
        let r = v.ranges.get(i)?;
        (r.first <= vpid.pageid).then_some((v, i))
    }

    /// Unshares range `i` of `v`, which the caller moved from PENDING to RUNNING, and wakes the waiters.
    fn run(&self, v: &VolumeRanges, i: usize) {
        let r = &v.ranges[i];
        let io = self.io_pagesize;
        let done = preallocate::unshare(&v.file, r.first as u64 * io, r.pages as u64 * io);
        let state = match done {
            Ok(()) => DONE,
            Err(e) => {
                self.first_error.lock().unwrap().get_or_insert(format!(
                    "unsharing pages {}..{} of volume {}: {e}",
                    r.first,
                    r.first as i64 + r.pages as i64,
                    v.volid
                ));
                FAILED
            }
        };
        v.state[i].store(state, Ordering::Release);
        let _g = self.lock.lock().unwrap();
        self.done.notify_all();
    }

    /// The write gate: returns once the range holding `vpid` (if any) is unshared; an error when it could not be.
    pub fn ensure(&self, vpid: Vpid) -> io::Result<()> {
        let Some((v, i)) = self.find(vpid) else {
            return Ok(());
        };
        let st = &v.state[i];
        let mut waited: Option<Instant> = None;
        loop {
            match st.load(Ordering::Acquire) {
                DONE => break,
                FAILED => {
                    let e = self.first_error.lock().unwrap().clone().unwrap_or_default();
                    return Err(io::Error::other(format!("before writing {vpid:?}: {e}")));
                }
                PENDING => {
                    if st
                        .compare_exchange(PENDING, RUNNING, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                    {
                        self.taken.fetch_add(1, Ordering::Relaxed);
                        self.run(v, i);
                    }
                }
                _ => {
                    waited.get_or_insert_with(Instant::now);
                    let mut g = self.lock.lock().unwrap();
                    while st.load(Ordering::Acquire) == RUNNING {
                        g = self.done.wait(g).unwrap();
                    }
                }
            }
        }
        if let Some(t) = waited {
            self.waits.fetch_add(1, Ordering::Relaxed);
            self.wait_ns
                .fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
        }
        Ok(())
    }

    /// Unshares every range no writer took, on up to `workers` threads, a volume at a time.
    pub fn spawn(self: &Arc<Self>, workers: usize) -> Vec<JoinHandle<()>> {
        let next = Arc::new(AtomicUsize::new(0));
        (0..workers.clamp(1, self.volumes.len().max(1)))
            .map(|_| {
                let (g, next) = (self.clone(), next.clone());
                std::thread::spawn(move || {
                    while let Some(v) = g.volumes.get(next.fetch_add(1, Ordering::Relaxed)) {
                        for (i, st) in v.state.iter().enumerate() {
                            if st
                                .compare_exchange(
                                    PENDING,
                                    RUNNING,
                                    Ordering::AcqRel,
                                    Ordering::Acquire,
                                )
                                .is_ok()
                            {
                                g.run(v, i);
                            }
                        }
                    }
                })
            })
            .collect()
    }

    /// Waits for the background threads; an error if any range could not be unshared.
    pub fn join(&self, handles: Vec<JoinHandle<()>>) -> io::Result<GateStats> {
        for h in handles {
            h.join()
                .map_err(|_| io::Error::other("background unshare thread panicked"))?;
        }
        if let Some(e) = self.first_error.lock().unwrap().clone() {
            return Err(io::Error::other(e));
        }
        Ok(GateStats {
            taken_by_writers: self.taken.load(Ordering::Relaxed),
            waits: self.waits.load(Ordering::Relaxed),
            wait_seconds: self.wait_ns.load(Ordering::Relaxed) as f64 / 1e9,
        })
    }
}
