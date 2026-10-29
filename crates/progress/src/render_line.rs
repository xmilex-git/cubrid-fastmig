//! The plain display: one line per print, for a pipe, a log file or CI. Written to stderr by the monitor.
//!
//! Input: the progress state, this tick's estimate, the heap reading. Output: lines such as
//! `[phase 3/8 volumes] 42.1% (198 MiB/470 MiB, 1.1 GiB/s) ETA phase 0:02 total ~0:05 done ~22:41 heap 2.1 MiB/max 3.0 MiB of --mem 16 GiB`
//! followed by ` (since last: volume-copy done 1.78 s 4.2 GiB 2.4 GiB/s, volume-paths done)` naming the phases
//! that ended since the previous line.
//! Source: user request relayed in workspace#472 (line format).

use std::sync::atomic::Ordering::Relaxed;
use std::time::Instant;

use crate::clock;
use crate::estimate::{Estimate, elapsed};
use crate::meter::{Phase, Progress};
use crate::unit::{self, Unit};

/// Current and maximum heap bytes (`None` without the counting allocator) and the memory limit (`--mem`), if any.
#[derive(Clone, Copy)]
pub struct Heap {
    pub now_max: Option<(u64, u64)>,
    pub memory_limit: Option<u64>,
    /// Temp disk bytes now, at most, and the limit (`--temp-disk`), when a limit is set.
    pub temp: Option<(u64, u64, u64)>,
}

impl Heap {
    pub fn text(&self) -> String {
        let mut s = match self.now_max {
            Some((now, max)) => format!(
                "heap {}/max {}",
                unit::bytes(now as f64),
                unit::bytes(max as f64)
            ),
            None => "heap n/a".into(),
        };
        if let Some(c) = self.memory_limit {
            s += &format!(" of --mem {}", unit::bytes(c as f64));
        }
        if let Some((now, max, limit)) = self.temp {
            s += &format!(
                " temp {}/max {} of --temp-disk {}",
                unit::bytes(now as f64),
                unit::bytes(max as f64),
                unit::bytes(limit as f64)
            );
        }
        s
    }
}

pub(crate) fn tag(p: &Progress, i: usize) -> String {
    format!(
        "[phase {}/{} {}]",
        i + 1,
        p.phases.len(),
        p.phases[i].spec.name
    )
}

/// `42.1% (198 MiB/470 MiB, 1.1 GiB/s)`; empty for a step.
pub(crate) fn amount(ph: &Phase, rate: Option<f64>) -> String {
    if ph.spec.unit == Unit::Step {
        return String::new();
    }
    let total = ph.total.load(Relaxed);
    let rate = rate.map_or(String::new(), |r| format!(", {}", ph.spec.unit.rate(r)));
    if total == 0 {
        return format!("({}{rate}) ", ph.spec.unit.amount(ph.done()));
    }
    // counters run past a rough total (fsync counts cloned files too): show it as full
    let done = ph.done().min(total);
    format!(
        "{:.1}% ({}{rate}) ",
        done as f64 * 100.0 / total as f64,
        ph.spec.unit.fraction(done, total)
    )
}

/// `ETA phase 0:02 total ~0:05 done ~22:41`.
pub(crate) fn eta(e: &Estimate) -> String {
    format!(
        "ETA phase {} total ~{} done ~{}",
        clock::duration(e.phase_eta),
        clock::duration(e.total_eta),
        clock::local_hhmm(e.total_eta)
    )
}

pub fn running(p: &Progress, i: usize, e: &Estimate, heap: &Heap) -> String {
    format!(
        "{} {}{} {}",
        tag(p, i),
        amount(&p.phases[i], e.rate),
        eta(e),
        heap.text()
    )
}

/// `0.41 s` under a minute, `m:ss` above.
pub(crate) fn secs(s: f64) -> String {
    if s < 60.0 {
        format!("{s:.2} s")
    } else {
        clock::duration(s)
    }
}

/// `(470 MiB, 1.1 GiB/s)` of a finished phase; empty for a step.
pub(crate) fn finished_amount(ph: &Phase, now: Instant) -> String {
    if ph.spec.unit == Unit::Step {
        return String::new();
    }
    let el = elapsed(ph, now);
    let done = ph.done();
    let rate = if el > 0.0 {
        format!(", {}", ph.spec.unit.rate(done as f64 / el))
    } else {
        String::new()
    };
    format!("({}{rate})", ph.spec.unit.amount(done))
}

/// `volume-copy done 1.78 s 4.2 GiB 2.4 GiB/s`, or `volume-paths done` for a step.
pub fn ended_brief(p: &Progress, i: usize, now: Instant) -> String {
    let ph = &p.phases[i];
    if ph.spec.unit == Unit::Step {
        return format!("{} done", ph.spec.name);
    }
    let el = elapsed(ph, now);
    let done = ph.done();
    let rate = if el > 0.0 {
        format!(" {}", ph.spec.unit.rate(done as f64 / el))
    } else {
        String::new()
    };
    format!(
        "{} done {} {}{rate}",
        ph.spec.name,
        secs(el),
        ph.spec.unit.amount(done)
    )
}

/// The last line: total time against the first estimate, and the heap peak.
pub fn summary(p: &Progress, heap: &Heap) -> String {
    let total = p.run_started.elapsed().as_secs_f64();
    let est = p.start_estimate.get().map_or(String::new(), |e| {
        format!(" (estimated at start ~{})", secs(*e))
    });
    let state = match p.phases.get(p.current()) {
        None => format!("[{}] finished in {}{est}", p.title, secs(total)),
        Some(_) => format!(
            "[{}] stopped in {} after {}",
            p.title,
            tag(p, p.current()),
            secs(total)
        ),
    };
    format!("{state} {}", heap.text())
}
