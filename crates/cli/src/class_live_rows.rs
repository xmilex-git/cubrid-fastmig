//! The tool's side of the engine check's row counts: the live rows of every class heap of the target, counted from
//! its volumes before any engine process opens it.
//!
//! Input: the target's first volume and active log. Output: class name (as the class object stores it, e.g.
//! `app_owner.customer`) → live rows of its own heap. A partitioned class and each of its partitions have a heap
//! each; `engine_check` adds them up where the engine counts them together.
//! How: the verifier's heap pass with no index (`fastmig_verify::heap_pass::scan_heaps`), so the count is the same
//! one `check` builds its live row map from, without `--check` having run.
//! Source: workspace#611 decision 3 (b) (the tool's count: the verifier's live rows, chosen in the ticket's
//! [VERIFY]).

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use fastmig_index::{MemoryLimit, SpillDir, WorkerLimit};
use fastmig_ondisk::volume_set::Locate;
use fastmig_verify::heap_pass::{HeapScanJob, HeapTarget, scan_heaps};
use fastmig_verify::{Database, Role};

pub fn count(
    first_volume: &Path,
    log: &Path,
    workers: usize,
    memory_bytes: u64,
    spill_dir: &Path,
) -> io::Result<BTreeMap<String, u64>> {
    let db = Database::open(first_volume, Locate::Recorded, log, Role::Target, None)?;
    let jobs: Vec<HeapScanJob> = db
        .classes
        .iter()
        .filter(|c| !c.class.heap.is_null())
        .map(|c| HeapScanJob {
            target: HeapTarget {
                name: db.class_name(c.oid),
                class_oid: c.oid,
                class: Some(&c.class),
                hfid: c.class.heap,
            },
            indexes: Vec::new(),
        })
        .collect();
    let wb = WorkerLimit::new(workers);
    let mb = MemoryLimit::new(usize::try_from(memory_bytes).unwrap_or(usize::MAX));
    let spill = SpillDir::create(spill_dir)?;
    let passes = scan_heaps(&db, &jobs, &wb, &mb, &spill, None)
        .map_err(|e| io::Error::other(format!("counting the target's rows: {e:?}")))?;
    Ok(jobs
        .iter()
        .zip(passes)
        .map(|(j, p)| (j.target.name.clone(), p.live.count()))
        .collect())
}
