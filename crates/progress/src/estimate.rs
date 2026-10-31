//! Throughput and remaining time, computed by the monitor from the counters (never by a worker).
//!
//! Input: the [`Progress`] state and a [`ProcSample`] at each tick. Output: per tick, the running phase's measured
//! rate (an exponential moving average over the last few seconds), its ETA, and the whole run's ETA.
//! Rules (user requests relayed in workspace#472 추가/추가 2, workspace#560):
//! - a phase not yet measured is estimated as `amount / default rate` ([`crate::default_rates`]), times the speed
//!   factor of its prior bottleneck (from its spec);
//! - a bottleneck's speed factor is measured time / default-predicted time over the work bound by it so far: the
//!   observations made before the phases (the precheck's pair count) and every phase that has run
//!   [`MIN_SAMPLE_S`], each classified by what it used ([`crate::bottleneck`]). A host whose CPU work runs 1.5x slower
//!   than the defaults makes the remaining CPU-bound phases 1.5x longer, and leaves the I/O-bound ones alone;
//!   a bottleneck nothing has been bound by yet keeps its default (factor 1); work whose use shows no bottleneck
//!   ([`crate::bottleneck`]) moves no factor;
//! - the running phase uses its own moving average once it has run [`MIN_SAMPLE_S`] and the rate is not ~0,
//!   otherwise its scaled default counted down by its time so far; a phase shorter than [`MIN_SAMPLE_S`] does not
//!   move a speed factor (a clone "copies" gigabytes in a millisecond);
//! - the run's ETA is always given, marked approximate by the caller.

use std::sync::atomic::Ordering::Relaxed;
use std::time::Instant;

use crate::bottleneck::Bottleneck;
use crate::default_rates::STEP;
use crate::meter::{Phase, Progress};
use crate::proc_sample::ProcSample;
use crate::unit::Unit;

/// Seconds a phase must run before its own rate is trusted.
pub const MIN_SAMPLE_S: f64 = 2.0;
/// Time constant of the moving average, in seconds.
const TAU_S: f64 = 3.0;
/// A speed factor stays within these bounds.
const FACTOR_RANGE: (f64, f64) = (0.05, 20.0);

/// What the display shows for one tick.
pub struct Estimate {
    /// The running phase's moving-average rate; `None` while the sample is too short.
    pub rate: Option<f64>,
    /// Seconds left in the running phase.
    pub phase_eta: f64,
    /// Seconds left in the whole run.
    pub total_eta: f64,
}

/// Per-phase moving averages, owned by the monitor thread.
pub struct Estimator {
    last: Option<(usize, Instant, u64)>,
    ema: f64,
}

impl Default for Estimator {
    fn default() -> Self {
        Estimator {
            last: None,
            ema: 0.0,
        }
    }
}

impl Estimator {
    pub fn tick(&mut self, p: &Progress, now: Instant, sample: Option<ProcSample>) -> Estimate {
        let cur = p.current();
        let Some(ph) = p.phases.get(cur) else {
            return Estimate {
                rate: None,
                phase_eta: 0.0,
                total_eta: 0.0,
            };
        };
        let done = ph.done();
        match self.last {
            Some((i, t, d)) if i == cur => {
                let dt = now.duration_since(t).as_secs_f64();
                if dt > 0.0 {
                    let inst = done.saturating_sub(d) as f64 / dt;
                    let alpha = 1.0 - (-dt / TAU_S).exp();
                    self.ema += alpha * (inst - self.ema);
                }
            }
            _ => {
                // a new phase: seed the average with its rate so far
                let el = elapsed(ph, now);
                self.ema = if el > 0.0 { done as f64 / el } else { 0.0 };
            }
        }
        self.last = Some((cur, now, done));

        let factors = speed_factors(p, now, sample);
        let el = elapsed(ph, now);
        let trusted = ph.spec.unit != Unit::Step && el >= MIN_SAMPLE_S && self.ema > 1e-9;
        let phase_eta = if trusted {
            remaining(ph) as f64 / self.ema
        } else if ph.spec.unit == Unit::Step {
            (STEP.as_secs_f64() - el).max(0.0)
        } else {
            // the whole phase's scaled default minus its time so far: counts down while the phase adds nothing yet
            // (the verifier's heap pass) instead of standing still
            let r = classified(ph, now, sample, p.worker_limit).unwrap_or(ph.spec.prior);
            (scaled_default(p, ph, factors.of(r)) - el).max(0.0)
        };
        let pending: f64 = p.phases[cur + 1..]
            .iter()
            .map(|q| scaled_default(p, q, factors.of(q.spec.prior)))
            .sum();
        Estimate {
            rate: trusted.then_some(self.ema),
            phase_eta,
            total_eta: phase_eta + pending,
        }
    }
}

/// Elapsed + estimated remaining seconds of the run when phase `from` begins, before it has done anything.
pub(crate) fn at_start(p: &Progress, from: usize, sample: Option<ProcSample>) -> f64 {
    let now = Instant::now();
    let el = now.duration_since(p.run_started).as_secs_f64();
    let factors = speed_factors(p, now, sample);
    el + p.phases[from..]
        .iter()
        .map(|q| scaled_default(p, q, factors.of(q.spec.prior)))
        .sum::<f64>()
}

/// Seconds phase `ph` has run (to its end if it ended).
pub(crate) fn elapsed(ph: &Phase, now: Instant) -> f64 {
    match (ph.started.get(), ph.ended.get()) {
        (Some(s), Some(e)) => e.duration_since(*s).as_secs_f64(),
        (Some(s), None) => now.duration_since(*s).as_secs_f64(),
        _ => 0.0,
    }
}

/// What phase `ph` used so far (to its end if it ended); `None` before it began or where /proc is unreadable.
pub(crate) fn used(ph: &Phase, sample_now: Option<ProcSample>) -> Option<ProcSample> {
    let start = (*ph.sample_start.get()?)?;
    let end = match ph.sample_end.get() {
        Some(s) => (*s)?,
        None => sample_now?,
    };
    Some(end.since(start))
}

/// The bottleneck phase `ph` showed once it has run [`MIN_SAMPLE_S`]; `None` before, where /proc is unreadable, or
/// when its use shows none.
pub(crate) fn classified(
    ph: &Phase,
    now: Instant,
    sample_now: Option<ProcSample>,
    workers: usize,
) -> Option<Bottleneck> {
    let el = elapsed(ph, now);
    match used(ph, sample_now) {
        Some(u) if el >= MIN_SAMPLE_S => Bottleneck::classify(u, el, workers, ph.device),
        _ => None,
    }
}

fn remaining(ph: &Phase) -> u64 {
    ph.total.load(Relaxed).saturating_sub(ph.done())
}

fn default_secs(p: &Progress, ph: &Phase, amount: u64) -> f64 {
    let rate = ph.spec.rate.with_workers(p.worker_limit);
    if rate > 0.0 {
        amount as f64 / rate
    } else {
        0.0
    }
}

/// A whole phase: a step takes its fixed time; measured work its default time times the factor.
fn scaled_default(p: &Progress, ph: &Phase, factor: f64) -> f64 {
    match ph.spec.unit {
        Unit::Step => STEP.as_secs_f64(),
        _ => default_secs(p, ph, ph.total.load(Relaxed)) * factor,
    }
}

/// Measured / default-predicted seconds per bottleneck; 1 for one nothing has been bound by yet.
pub(crate) struct SpeedFactors([f64; 3]);

impl SpeedFactors {
    pub fn of(&self, r: Bottleneck) -> f64 {
        self.0[r as usize]
    }
}

pub(crate) fn speed_factors(
    p: &Progress,
    now: Instant,
    sample_now: Option<ProcSample>,
) -> SpeedFactors {
    let mut real = [0.0; 3];
    let mut predicted = [0.0; 3];
    let observations = p.observations.lock().unwrap_or_else(|e| e.into_inner());
    for o in observations.iter() {
        if o.seconds > 0.0
            && o.predicted_s > 0.0
            && let Some(r) = o.bottleneck(p.worker_limit)
        {
            real[r as usize] += o.seconds;
            predicted[r as usize] += o.predicted_s;
        }
    }
    for ph in p.phases.iter().filter(|q| q.spec.unit != Unit::Step) {
        let el = elapsed(ph, now);
        let pred = default_secs(p, ph, ph.done());
        if pred > 0.0
            && let Some(r) = classified(ph, now, sample_now, p.worker_limit)
        {
            real[r as usize] += el;
            predicted[r as usize] += pred;
        }
    }
    let mut f = [1.0; 3];
    for r in Bottleneck::ALL {
        let i = r as usize;
        if predicted[i] > 0.0 {
            f[i] = (real[i] / predicted[i]).clamp(FACTOR_RANGE.0, FACTOR_RANGE.1);
        }
    }
    SpeedFactors(f)
}
