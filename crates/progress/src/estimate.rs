//! Throughput and remaining time, computed by the monitor from the counters (never by a worker).
//!
//! Input: the [`Progress`] state at each tick. Output: per tick, the running phase's measured rate (an
//! exponential moving average over the last few seconds), its ETA, and the whole run's ETA.
//! Rules (user request relayed in workspace#472, 추가/추가 2):
//! - a phase not yet measured is estimated as `amount / default rate` ([`crate::default_rates`]), times the
//!   run's speed factor: measured time / default-predicted time over the phases measured so far (a host 1.5x
//!   slower than the defaults makes the remaining phases 1.5x longer);
//! - the running phase uses its own moving average once it has run [`MIN_SAMPLE_S`] and the rate is not ~0,
//!   otherwise the scaled default; a phase shorter than [`MIN_SAMPLE_S`] does not move the speed factor (a clone
//!   "copies" gigabytes in a millisecond);
//! - the run's ETA is always given, marked approximate by the caller.

use std::time::Instant;

use crate::default_rates::STEP;
use crate::meter::{Phase, Progress};
use crate::unit::Unit;

/// Seconds a phase must run before its own rate is trusted.
pub const MIN_SAMPLE_S: f64 = 2.0;
/// Time constant of the moving average, in seconds.
const TAU_S: f64 = 3.0;
/// The speed factor stays within these bounds.
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
    pub fn tick(&mut self, p: &Progress, now: Instant) -> Estimate {
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

        let factor = speed_factor(p, now);
        let el = elapsed(ph, now);
        let trusted = ph.spec.unit != Unit::Step && el >= MIN_SAMPLE_S && self.ema > 1e-9;
        let phase_eta = if trusted {
            remaining(ph) as f64 / self.ema
        } else if ph.spec.unit == Unit::Step {
            (STEP.as_secs_f64() - el).max(0.0)
        } else {
            default_secs(p, ph, remaining(ph)) * factor
        };
        let pending: f64 = p.phases[cur + 1..]
            .iter()
            .map(|q| scaled_default(p, q, factor))
            .sum();
        Estimate {
            rate: trusted.then_some(self.ema),
            phase_eta,
            total_eta: phase_eta + pending,
        }
    }
}

/// Elapsed + estimated remaining seconds of the run when phase `from` begins, before anything is measured.
pub(crate) fn at_start(p: &Progress, from: usize) -> f64 {
    let el = p.run_started.elapsed().as_secs_f64();
    el + p.phases[from..]
        .iter()
        .map(|q| scaled_default(p, q, 1.0))
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

fn remaining(ph: &Phase) -> u64 {
    ph.total
        .load(std::sync::atomic::Ordering::Relaxed)
        .saturating_sub(ph.done())
}

fn default_secs(p: &Progress, ph: &Phase, amount: u64) -> f64 {
    let rate = ph.spec.rate.with_workers(p.worker_limit);
    if rate > 0.0 {
        amount as f64 / rate
    } else {
        0.0
    }
}

/// A whole unstarted phase: a step takes its fixed time; measured work its default time times the factor.
fn scaled_default(p: &Progress, ph: &Phase, factor: f64) -> f64 {
    match ph.spec.unit {
        Unit::Step => STEP.as_secs_f64(),
        _ => default_secs(p, ph, ph.total.load(std::sync::atomic::Ordering::Relaxed)) * factor,
    }
}

/// Measured / default-predicted seconds over the measured phases long enough to count; 1 before any.
fn speed_factor(p: &Progress, now: Instant) -> f64 {
    let (mut real, mut predicted) = (0.0, 0.0);
    for ph in p.phases.iter().filter(|q| q.spec.unit != Unit::Step) {
        let el = elapsed(ph, now);
        let pred = default_secs(p, ph, ph.done());
        if el >= MIN_SAMPLE_S && pred > 0.0 {
            real += el;
            predicted += pred;
        }
    }
    if predicted > 0.0 {
        (real / predicted).clamp(FACTOR_RANGE.0, FACTOR_RANGE.1)
    } else {
        1.0
    }
}
