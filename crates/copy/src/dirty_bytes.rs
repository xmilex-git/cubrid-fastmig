//! Bytes the whole system still has to write to its devices: `Dirty` plus `Writeback` of `/proc/meminfo`.
//!
//! Input: none. Output: the byte count, or `None` when `/proc/meminfo` cannot be read or lacks the two lines.
//! Used by `sync_progress` to count a sync pass as the devices write, whoever submitted the writes (the kernel's
//! flusher writes the run's older dirty pages at the same time, workspace#606). System-wide, so other processes'
//! writes mix in; the caller only lets its count grow. Every kernel the tool runs on has both lines (RHEL/Rocky 8's
//! 4.18 too), and reading them needs no privilege.
//! Source: Linux `proc(5)` (`Dirty`, `Writeback`, in kB).

/// `Dirty` + `Writeback` of `/proc/meminfo`, in bytes.
pub fn system() -> Option<u64> {
    let info = std::fs::read_to_string("/proc/meminfo").ok()?;
    let kb = |key: &str| {
        info.lines()
            .find_map(|l| l.strip_prefix(key))
            .and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
    };
    Some((kb("Dirty:")? + kb("Writeback:")?) << 10)
}
