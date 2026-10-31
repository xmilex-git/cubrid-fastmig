//! Bounds the spill files one merge opens at once: while an index has more spilled runs than a worker's merge
//! memory holds read buffers for, its smallest runs are merged into one (an intermediate merge).
//!
//! Input: an index's runs, its key order, the spill directory, and the merge memory of one worker. Output: the
//! same pairs in at most [`max_open`] spilled runs. An input run's space is freed as it is read and the output's
//! is taken as it is written (`SpillCursor::consume`, `SpillOut`), so the spill bytes on disk never exceed those
//! of the runs before the merge: the temp disk bound (`temp_disk`) holds.
//! Memory: a merge of `k` runs holds `k` read buffers and cursors and one write buffer, at most [`max_open`] ×
//! [`open_cost`] — one worker's merge memory — unless that is under three open runs (a key too long for it), when
//! it still merges two at a time.
//! Source: workspace#526 (1번 세션 decision (B): merge buffers inside --mem, a multi-pass merge bounds the open
//! runs); engine counterpart `external_sort.c` `sort_exphase_merge` (merges runs in passes of the buffers it has).

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::io;

use crate::key_order::KeyOrder;
use crate::kway_merge::{Cursor, KeyStream, KwayMerge};
use crate::run_writer::IndexRuns;
use crate::spill_dir::SpillDir;
use crate::spill_run::{SpillCursor, SpillOut, SpillRun};
use fastmig_progress::Meter;

/// Pairs pushed between two adds to the progress meter.
const COUNT_BATCH: u64 = 1 << 16;

/// Read buffer of one open spill run in a merge; also the write buffer of a spill.
pub const MERGE_BUFFER: usize = 64 << 10;

/// One open run besides its buffer and key: the cursor, its reader and its merge heap entry.
const CURSOR_BYTES: usize = 256;

/// Merge memory each worker holds from before the heap scan: three open runs of short keys, so an intermediate
/// merge always has two inputs and an output.
pub const MERGE_FLOOR: usize = 3 * (MERGE_BUFFER + CURSOR_BYTES + 256);

/// The memory limit per worker the index build needs at least: its merge floor, and a heap scan share (half the
/// limit over the workers) that holds the spill write buffer and a run of as many bytes.
pub const WORKER_MIN: usize = MERGE_FLOOR + 2 * crate::heap_scan::SCAN_MIN;

/// Memory one open spill run of `runs` takes in a merge: its read buffer, cursor and longest key.
pub fn open_cost(runs: &IndexRuns) -> usize {
    MERGE_BUFFER
        + CURSOR_BYTES
        + runs
            .spilled
            .iter()
            .map(SpillRun::max_key)
            .max()
            .unwrap_or(0)
}

/// Spilled runs one merge may open within `budget` bytes, never fewer than three.
pub fn max_open(budget: usize, cost: usize) -> usize {
    (budget / cost).max(3)
}

/// Merges the smallest spilled runs of `runs`, as many at a time as `budget` allows, until at most
/// [`max_open`] remain. In-memory runs are left alone.
/// `progress` counts each spilled run's pairs once (the verifier's sort step, workspace#560): a run no merge reads at
/// once, a run a merge reads first along that merge's pushes, so the count moves with the time spent.
pub fn bound_open_runs(
    runs: &mut IndexRuns,
    order: &dyn KeyOrder,
    dir: &SpillDir,
    budget: usize,
    progress: Option<Meter>,
) -> io::Result<()> {
    let add = |n: u64| {
        if let Some(m) = &progress {
            m.add(n);
        }
    };
    let cap = max_open(budget, open_cost(runs));
    if runs.spilled.len() <= cap {
        add(runs.spilled.iter().map(|r| r.len() as u64).sum());
        return Ok(());
    }
    // smallest first, kept in a heap: re-sorting every round is O(n² log n) in the runs (workspace#526)
    let mut heap: BinaryHeap<BySize> = std::mem::take(&mut runs.spilled)
        .into_iter()
        .map(|r| BySize(r, false))
        .collect();
    while heap.len() > cap {
        // only as many as bring the count to the cap: the fewest bytes merged twice
        let k = (cap - 1).min(heap.len() - cap + 1);
        let popped: Vec<BySize> = (0..k).map(|_| heap.pop().unwrap()).collect();
        // this merge's pushes, and the pairs of its runs not counted yet, counted along them
        let pushes: u64 = popped.iter().map(|r| r.0.len() as u64).sum();
        let fresh: u64 = popped
            .iter()
            .filter(|r| !r.1)
            .map(|r| r.0.len() as u64)
            .sum();
        let cursors = popped
            .into_iter()
            .map(|r| SpillCursor::consume(r.0, MERGE_BUFFER).map(Cursor::File))
            .collect::<io::Result<Vec<_>>>()?;
        let mut stream = KwayMerge::new(cursors, order);
        let mut out = SpillOut::new(dir, MERGE_BUFFER)?;
        let (mut pushed, mut counted) = (0u64, 0u64);
        while let Some((key, oid)) = stream.next_pair()? {
            out.push(key, oid)?;
            pushed += 1;
            if pushed % COUNT_BATCH == 0 {
                let due = (fresh as u128 * pushed as u128 / pushes.max(1) as u128) as u64;
                add(due - counted);
                counted = due;
            }
        }
        add(fresh - counted);
        heap.push(BySize(out.finish()?, true));
    }
    add(heap.iter().filter(|r| !r.1).map(|r| r.0.len() as u64).sum());
    runs.spilled = heap.into_iter().map(|r| r.0).collect();
    Ok(())
}

/// A spilled run ordered smallest first in a max-heap, and whether its pairs were counted.
struct BySize(SpillRun, bool);

impl PartialEq for BySize {
    fn eq(&self, o: &Self) -> bool {
        self.0.bytes() == o.0.bytes()
    }
}
impl Eq for BySize {}
impl PartialOrd for BySize {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for BySize {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        Reverse(self.0.bytes()).cmp(&Reverse(o.0.bytes()))
    }
}
