//! What the process has used so far: CPU seconds and bytes read from and written to storage devices. Read at each
//! phase boundary (by the thread that calls `advance`) and at each monitor tick, never by a worker, so a phase's use
//! is the difference of two samples (workspace#560: the reader does the work).
//!
//! Input: `/proc/self/stat` (utime, stime) and `/proc/self/io` (read_bytes, write_bytes: what reached the block
//! layer, page cache hits excluded). Output: [`ProcSample`]; `None` where /proc cannot be read (the estimate then
//! keeps each phase's prior bottleneck).
//! Source: proc(5) `/proc/pid/stat` fields 14-15, `/proc/pid/io`.

/// The kernel reports utime/stime in clock ticks of USER_HZ, 100 on every Linux ABI this tool builds for.
const USER_HZ: f64 = 100.0;

#[derive(Clone, Copy, Debug, Default)]
pub struct ProcSample {
    pub cpu_s: f64,
    pub read_bytes: u64,
    pub write_bytes: u64,
}

impl ProcSample {
    pub fn now() -> Option<ProcSample> {
        let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
        // fields after the parenthesised command name: state is field 3, utime 14, stime 15
        let rest = &stat[stat.rfind(')')? + 1..];
        let f: Vec<&str> = rest.split_whitespace().collect();
        let ticks = f.get(11)?.parse::<u64>().ok()? + f.get(12)?.parse::<u64>().ok()?;
        let io = std::fs::read_to_string("/proc/self/io").ok()?;
        let field = |name: &str| {
            io.lines()
                .find_map(|l| l.strip_prefix(name))
                .and_then(|v| v.trim().parse::<u64>().ok())
        };
        Some(ProcSample {
            cpu_s: ticks as f64 / USER_HZ,
            read_bytes: field("read_bytes:")?,
            write_bytes: field("write_bytes:")?,
        })
    }

    /// What was used between `earlier` and `self`.
    pub fn since(self, earlier: ProcSample) -> ProcSample {
        ProcSample {
            cpu_s: (self.cpu_s - earlier.cpu_s).max(0.0),
            read_bytes: self.read_bytes.saturating_sub(earlier.read_bytes),
            write_bytes: self.write_bytes.saturating_sub(earlier.write_bytes),
        }
    }
}
