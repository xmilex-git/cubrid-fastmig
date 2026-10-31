//! The shared progress state: the run's phases in order, each with its total and its per-thread done counters,
//! and the phase boundaries.
//!
//! Input: the phase list (from the command), totals as the plan learns them, `advance` at each boundary, and
//! [`Meter::add`] from the workers. Output: what the monitor and the report read.
//! Writers pay one `Relaxed` add on their own cache line per batch (a copy task, a run of records): no lock, no
//! clock, no allocation. Sums, times and rates are the reader's work. Boundaries are timed by the thread that
//! calls `advance`, which is never a worker's hot loop. It also samples the process's CPU time and device bytes there
//! ([`crate::proc_sample`]), two small /proc reads per boundary.
//! Source: user request relayed in workspace#472 (추가 3: writers free, the reader does the work).

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::Thread;
use std::time::Instant;

use crate::bottleneck::{Bottleneck, DeviceThroughput};
use crate::default_rates::DefaultRate;
use crate::proc_sample::ProcSample;
use crate::unit::Unit;

/// Counter slots per phase. Threads beyond this share slots (still correct, only no longer contention-free).
const SLOTS: usize = 64;

/// One thread's counter, alone on its cache line so two writers never share one.
#[repr(align(64))]
#[derive(Default)]
pub(crate) struct Slot(AtomicU64);

#[derive(Clone, Copy, Debug)]
pub struct PhaseSpec {
    pub name: &'static str,
    pub unit: Unit,
    /// Ignored for [`Unit::Step`], which is timed by [`crate::default_rates::STEP`].
    pub rate: DefaultRate,
    /// The bottleneck assumed until the phase has run long enough to be classified ([`crate::bottleneck`]).
    pub prior: Bottleneck,
    /// A second part of the phase, counted apart ([`Meter::second`]) in its own unit at its own rate: the index
    /// build's tree build after its heap scan, the verifier's merge after its heap pass (workspace#560). The two may
    /// alternate (batches).
    pub second: Option<(Unit, DefaultRate)>,
}

impl PhaseSpec {
    /// A short fixed step with no amount.
    pub const fn step(name: &'static str) -> PhaseSpec {
        PhaseSpec {
            name,
            unit: Unit::Step,
            rate: DefaultRate { points: &[] },
            prior: Bottleneck::Cpu,
            second: None,
        }
    }

    pub const fn work(
        name: &'static str,
        unit: Unit,
        rate: DefaultRate,
        prior: Bottleneck,
    ) -> PhaseSpec {
        PhaseSpec {
            name,
            unit,
            rate,
            prior,
            second: None,
        }
    }

    /// Work in two parts at two rates, shown as one phase: `unit` and `rate` for the part counted by the phase's
    /// meter, `second` for the part counted by [`Meter::second`].
    pub const fn two_parts(
        name: &'static str,
        unit: Unit,
        rate: DefaultRate,
        second: (Unit, DefaultRate),
        prior: Bottleneck,
    ) -> PhaseSpec {
        PhaseSpec {
            name,
            unit,
            rate,
            prior,
            second: Some(second),
        }
    }
}

pub(crate) struct Phase {
    pub spec: PhaseSpec,
    /// Per part (the second used only with [`PhaseSpec::second`]): the total and the per-thread done counters.
    pub totals: [AtomicU64; 2],
    slots: [Box<[Slot]>; 2],
    pub started: OnceLock<Instant>,
    pub ended: OnceLock<Instant>,
    /// What the phase's byte rate is compared with to classify it ([`crate::bottleneck`]).
    pub device: DeviceThroughput,
    /// What the process had used when the phase began and ended (`None` inside: /proc unreadable).
    pub sample_start: OnceLock<Option<ProcSample>>,
    pub sample_end: OnceLock<Option<ProcSample>>,
    /// The longest stretch, in ms, the running phase's counters did not move, as the monitor saw it: over 2 s, some
    /// work of the phase is not counted (workspace#560). `u64::MAX` until the monitor sees the phase run.
    pub idle_max_ms: AtomicU64,
}

impl Phase {
    /// Done in part `k`.
    pub fn done_of(&self, k: usize) -> u64 {
        self.slots[k].iter().map(|s| s.0.load(Relaxed)).sum()
    }

    /// Done in the phase's own unit (its first part).
    pub fn done(&self) -> u64 {
        self.done_of(0)
    }

    /// The total in the phase's own unit (its first part).
    pub fn total(&self) -> u64 {
        self.totals[0].load(Relaxed)
    }
}

/// One piece of work timed outside the phases ([`Progress::observe`]).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Observation {
    pub seconds: f64,
    pub predicted_s: f64,
    pub used: Option<ProcSample>,
    pub device: DeviceThroughput,
}

impl Observation {
    /// Its bottleneck from what it used; CPU (the pair count's prior) where /proc is unreadable.
    pub fn bottleneck(&self, workers: usize) -> Option<Bottleneck> {
        self.used.map_or(Some(Bottleneck::Cpu), |u| {
            Bottleneck::classify(u, self.seconds, workers, self.device)
        })
    }
}

pub struct Progress {
    pub title: String,
    pub worker_limit: usize,
    pub(crate) phases: Vec<Phase>,
    /// Index of the running phase; `phases.len()` once all have ended.
    current: AtomicUsize,
    pub(crate) run_started: Instant,
    /// Elapsed + estimated remaining seconds when the first measured phase began (the plan's totals known).
    pub(crate) start_estimate: OnceLock<f64>,
    /// Work timed before the phases that predicts them (the precheck's pair count, workspace#560).
    pub(crate) observations: Mutex<Vec<Observation>>,
    /// (seconds since the run started, estimated total seconds) at each monitor tick: the report's ETA trail.
    pub(crate) trail: Mutex<Vec<(f64, f64)>>,
    /// The monitor thread, woken at each boundary (and by a warning) so the screen redraws at once.
    pub(crate) waker: OnceLock<Thread>,
    /// Warnings for the running monitor to print between its frames; `None` while no monitor runs, so
    /// [`Progress::warn`] prints directly.
    pub(crate) warnings: Mutex<Option<Vec<String>>>,
}

impl Progress {
    /// Starts the run at `started` (the clock and the first phase): a command that worked before its display began
    /// (the plan, its precheck) passes when that work began, so the estimate and the total time include it.
    /// `devices` holds each phase's device throughput by position; a phase past its end gets
    /// [`DeviceThroughput::NVME`].
    pub fn new(
        title: &str,
        specs: &[PhaseSpec],
        devices: &[DeviceThroughput],
        worker_limit: usize,
        started: Instant,
    ) -> Arc<Progress> {
        let now = started;
        let phases: Vec<Phase> = specs
            .iter()
            .enumerate()
            .map(|(i, &spec)| Phase {
                spec,
                totals: [AtomicU64::new(0), AtomicU64::new(0)],
                slots: [0, 1].map(|_| (0..SLOTS).map(|_| Slot::default()).collect()),
                started: OnceLock::new(),
                ended: OnceLock::new(),
                device: devices.get(i).copied().unwrap_or(DeviceThroughput::NVME),
                sample_start: OnceLock::new(),
                sample_end: OnceLock::new(),
                idle_max_ms: AtomicU64::new(u64::MAX),
            })
            .collect();
        if let Some(p) = phases.first() {
            let _ = p.started.set(now);
            // the process's counters start at 0 with the process, which is when the first phase's work began
            let _ = p.sample_start.set(Some(ProcSample::default()));
        }
        Arc::new(Progress {
            title: title.to_string(),
            worker_limit,
            phases,
            current: AtomicUsize::new(0),
            run_started: now,
            start_estimate: OnceLock::new(),
            observations: Mutex::new(Vec::new()),
            trail: Mutex::new(Vec::new()),
            waker: OnceLock::new(),
            warnings: Mutex::new(None),
        })
    }

    /// The handle workers add to for phase `i`.
    pub fn meter(&self, i: usize) -> Meter<'_> {
        let [a, b] = &self.phases[i].slots;
        Meter {
            slots: a,
            second: b,
        }
    }

    /// Sets phase `i`'s total once the plan knows it; a phase without one counts 0 in the estimate.
    pub fn set_total(&self, i: usize, total: u64) {
        self.phases[i].totals[0].store(total, Relaxed);
    }

    /// Sets the total of phase `i`'s second part ([`PhaseSpec::two_parts`]).
    pub fn set_second_total(&self, i: usize, total: u64) {
        self.phases[i].totals[1].store(total, Relaxed);
    }

    /// Ends the running phase and begins the next.
    pub fn advance(&self) {
        let now = Instant::now();
        let i = self.current.load(Relaxed);
        let Some(p) = self.phases.get(i) else { return };
        let sample = ProcSample::now();
        let _ = p.ended.set(now);
        let _ = p.sample_end.set(sample);
        if let Some(next) = self.phases.get(i + 1) {
            let _ = next.started.set(now);
            let _ = next.sample_start.set(sample);
            if next.spec.unit != Unit::Step && self.start_estimate.get().is_none() {
                let est = crate::estimate::at_start(self, i + 1, sample);
                let _ = self.start_estimate.set(est);
            }
        }
        self.current.store(i + 1, Relaxed);
        if let Some(t) = self.waker.get() {
            t.unpark();
        }
    }

    /// Records work timed before the phases that tells how fast this host runs a kind of later work: `seconds` it
    /// took, `predicted_s` its default rate gives, `used` the [`ProcSample`] difference over it and `device` the
    /// throughput of what it read (which bottleneck it had). Call before the phase it predicts begins.
    pub fn observe(
        &self,
        seconds: f64,
        predicted_s: f64,
        used: Option<ProcSample>,
        device: DeviceThroughput,
    ) {
        let mut o = self.observations.lock().unwrap_or_else(|e| e.into_inner());
        o.push(Observation {
            seconds,
            predicted_s,
            used,
            device,
        });
    }

    /// Ends every phase still open (the run is over).
    pub fn finish(&self) {
        while self.current.load(Relaxed) < self.phases.len() {
            self.advance();
        }
    }

    /// Prints a warning line on stderr at once: through the monitor while one runs (the screen is cleared,
    /// the warning written above it, the screen redrawn), directly otherwise.
    pub fn warn(&self, msg: String) {
        let mut q = self.warnings.lock().unwrap_or_else(|e| e.into_inner());
        match q.as_mut() {
            Some(q) => {
                q.push(msg);
                if let Some(t) = self.waker.get() {
                    t.unpark();
                }
            }
            None => eprintln!("{msg}"),
        }
    }

    /// Takes the queued warnings; `Some` while a monitor runs. `close` hands printing back to [`Progress::warn`].
    pub(crate) fn take_warnings(&self, close: bool) -> Vec<String> {
        let mut q = self.warnings.lock().unwrap_or_else(|e| e.into_inner());
        let out = q.as_mut().map(std::mem::take).unwrap_or_default();
        if close {
            *q = None;
        }
        out
    }

    pub fn current(&self) -> usize {
        self.current.load(Relaxed)
    }
}

/// A phase's counter handle, cheap to copy into worker closures.
#[derive(Clone, Copy)]
pub struct Meter<'a> {
    slots: &'a [Slot],
    second: &'a [Slot],
}

static NEXT_SLOT: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static MY_SLOT: Cell<usize> = const { Cell::new(usize::MAX) };
}

impl<'a> Meter<'a> {
    /// The handle of the phase's second part ([`PhaseSpec::two_parts`]).
    pub fn second(self) -> Meter<'a> {
        Meter {
            slots: self.second,
            second: self.second,
        }
    }

    /// Adds `n` units of finished work. Call once per batch, not per item.
    #[inline]
    pub fn add(&self, n: u64) {
        self.slots[slot_index()].0.fetch_add(n, Relaxed);
    }
}

/// This thread's slot, assigned round-robin on its first add.
#[inline]
fn slot_index() -> usize {
    MY_SLOT.with(|s| {
        let v = s.get();
        if v != usize::MAX {
            return v;
        }
        let v = NEXT_SLOT.fetch_add(1, Relaxed) % SLOTS;
        s.set(v);
        v
    })
}
