//! The shared progress state: the run's phases in order, each with its total and its per-thread done counters,
//! and the phase boundaries.
//!
//! Input: the phase list (from the command), totals as the plan learns them, `advance` at each boundary, and
//! [`Meter::add`] from the workers. Output: what the monitor and the report read.
//! Writers pay one `Relaxed` add on their own cache line per batch (a copy task, a run of records): no lock, no
//! clock, no allocation. Sums, times and rates are the reader's work. Boundaries are timed by the thread that
//! calls `advance`, which is never a worker's hot loop.
//! Source: user request relayed in workspace#472 (추가 3: writers free, the reader does the work).

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::Thread;
use std::time::Instant;

use crate::default_rates::DefaultRate;
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
}

impl PhaseSpec {
    /// A short fixed step with no amount.
    pub const fn step(name: &'static str) -> PhaseSpec {
        PhaseSpec {
            name,
            unit: Unit::Step,
            rate: DefaultRate {
                per_worker: 0.0,
                max_workers: 1,
            },
        }
    }

    pub const fn work(name: &'static str, unit: Unit, rate: DefaultRate) -> PhaseSpec {
        PhaseSpec { name, unit, rate }
    }
}

pub(crate) struct Phase {
    pub spec: PhaseSpec,
    pub total: AtomicU64,
    slots: Box<[Slot]>,
    pub started: OnceLock<Instant>,
    pub ended: OnceLock<Instant>,
}

impl Phase {
    pub fn done(&self) -> u64 {
        self.slots.iter().map(|s| s.0.load(Relaxed)).sum()
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
    /// The monitor thread, woken at each boundary (and by a warning) so the screen redraws at once.
    pub(crate) waker: OnceLock<Thread>,
    /// Warnings for the running monitor to print between its frames; `None` while no monitor runs, so
    /// [`Progress::warn`] prints directly.
    pub(crate) warnings: Mutex<Option<Vec<String>>>,
}

impl Progress {
    /// Starts the run: the clock and the first phase.
    pub fn new(title: &str, specs: &[PhaseSpec], worker_limit: usize) -> Arc<Progress> {
        let now = Instant::now();
        let phases: Vec<Phase> = specs
            .iter()
            .map(|&spec| Phase {
                spec,
                total: AtomicU64::new(0),
                slots: (0..SLOTS).map(|_| Slot::default()).collect(),
                started: OnceLock::new(),
                ended: OnceLock::new(),
            })
            .collect();
        if let Some(p) = phases.first() {
            let _ = p.started.set(now);
        }
        Arc::new(Progress {
            title: title.to_string(),
            worker_limit,
            phases,
            current: AtomicUsize::new(0),
            run_started: now,
            start_estimate: OnceLock::new(),
            waker: OnceLock::new(),
            warnings: Mutex::new(None),
        })
    }

    /// The handle workers add to for phase `i`.
    pub fn meter(&self, i: usize) -> Meter<'_> {
        Meter {
            slots: &self.phases[i].slots,
        }
    }

    /// Sets phase `i`'s total once the plan knows it; a phase without one counts 0 in the estimate.
    pub fn set_total(&self, i: usize, total: u64) {
        self.phases[i].total.store(total, Relaxed);
    }

    /// Ends the running phase and begins the next.
    pub fn advance(&self) {
        let now = Instant::now();
        let i = self.current.load(Relaxed);
        let Some(p) = self.phases.get(i) else { return };
        let _ = p.ended.set(now);
        if let Some(next) = self.phases.get(i + 1) {
            let _ = next.started.set(now);
            if next.spec.unit != Unit::Step && self.start_estimate.get().is_none() {
                let est = crate::estimate::at_start(self, i + 1);
                let _ = self.start_estimate.set(est);
            }
        }
        self.current.store(i + 1, Relaxed);
        if let Some(t) = self.waker.get() {
            t.unpark();
        }
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
}

static NEXT_SLOT: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static MY_SLOT: Cell<usize> = const { Cell::new(usize::MAX) };
}

impl Meter<'_> {
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
