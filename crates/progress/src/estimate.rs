//! Throughput and remaining time, computed by the monitor from the counters (never by a worker).
//!
//! Input: the [`Progress`] state and a [`ProcSample`] at each tick. Output: per tick, the running phase's shown
//! rate (an exponential moving average over the last few seconds), its ETA, and the whole run's ETA.
//! Rules (user requests relayed in workspace#472 추가/추가 2; the ETA rule decided by the map supervisor for
//! workspace#560, 2026-10-31):
//! - a phase's default seconds are its amount over its default rate ([`crate::default_rates`]), summed over its
//!   parts for a phase in two ([`crate::meter::PhaseSpec::two_parts`]); its share done is its done default seconds
//!   over those;
//! - a bottleneck's speed factor is measured time / default seconds over the work bound by it so far: the
//!   observations made before the phases (the precheck's pair count) and every phase that has ended after running
//!   [`MIN_SAMPLE_S`] (a running phase's counter may run ahead of its time), each classified by what it used
//!   ([`crate::bottleneck`]). A host whose CPU work runs 1.5x slower than the defaults makes the remaining CPU-bound
//!   phases 1.5x longer, and leaves the I/O-bound ones alone; a bottleneck nothing has been bound by yet keeps its
//!   default (factor 1); work whose use shows no bottleneck moves no factor;
//! - a phase not yet begun takes its default seconds times its prior bottleneck's factor;
//! - the running phase takes its default seconds left times a blend of that factor and its own seconds per default
//!   second, weighted [`PRIOR_WEIGHT`] against the share done: the prior first, the phase's own observation as it
//!   gets done. No moving average, no floor: a good prior is not overwritten by a noisy rate;
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
/// Weight of the bottleneck's factor against the running phase's own observation, in fractions of the phase done:
/// the observation counts as much as the prior once this share is done. 0.5 from the workspace#560 replay of passes
/// 2-4 (worst sample 14.2 / 11.9 / 11.7 / 11.4% at 0.1 / 0.25 / 0.5 / 1.0, every other run within 9.2% at 0.5).
const PRIOR_WEIGHT: f64 = 0.5;

/// What the display shows for one tick.
pub struct Estimate {
    /// The running phase's moving-average rate; `None` while the sample is too short.
    pub rate: Option<f64>,
    /// Seconds left in the running phase.
    pub phase_eta: f64,
    /// Seconds left in the whole run.
    pub total_eta: f64,
}

/// The running phase's moving average of default seconds done per second, owned by the monitor thread: the rate
/// shown; the ETA does not use it (workspace#560).
pub struct Estimator {
    last: Option<(usize, Instant, f64)>,
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
        let done = done_secs(p, ph);
        match self.last {
            Some((i, t, d)) if i == cur => {
                let dt = now.duration_since(t).as_secs_f64();
                if dt > 0.0 {
                    let inst = (done - d).max(0.0) / dt;
                    let alpha = 1.0 - (-dt / TAU_S).exp();
                    self.ema += alpha * (inst - self.ema);
                }
            }
            _ => {
                // a new phase: seed the average with its rate so far
                let el = elapsed(ph, now);
                self.ema = if el > 0.0 { done / el } else { 0.0 };
            }
        }
        self.last = Some((cur, now, done));

        let factors = speed_factors(p, now, sample);
        let el = elapsed(ph, now);
        let phase_eta = if ph.spec.unit == Unit::Step {
            (STEP.as_secs_f64() - el).max(0.0)
        } else {
            // the bottleneck's factor at first, the phase's own seconds per default second as it gets done
            let total = total_secs(p, ph);
            let prior =
                factors.of(classified(ph, now, sample, p.worker_limit).unwrap_or(ph.spec.prior));
            let f = if total > 0.0 {
                (done / total).min(1.0)
            } else {
                0.0
            };
            let k = if done > 0.0 {
                (PRIOR_WEIGHT * prior + f * (el / done)) / (PRIOR_WEIGHT + f)
            } else {
                prior
            };
            (k * (total - done)).max(0.0)
        };
        let pending: f64 = p.phases[cur + 1..]
            .iter()
            .map(|q| scaled_default(p, q, factors.of(q.spec.prior)))
            .sum();
        Estimate {
            // the moving average in the phase's unit, shown only: one rate, so not for a phase in two parts
            rate: (ph.spec.unit != Unit::Step
                && el >= MIN_SAMPLE_S
                && self.ema > 1e-9
                && ph.spec.second.is_none())
            .then(|| self.ema * ph.spec.rate.with_workers(p.worker_limit)),
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

/// Seconds the defaults give `amounts` of phase `ph`'s parts.
fn default_secs(p: &Progress, ph: &Phase, amounts: [u64; 2]) -> f64 {
    [Some(ph.spec.rate), ph.spec.second.map(|(_, r)| r)]
        .into_iter()
        .zip(amounts)
        .map(|(r, a)| match r.map(|r| r.with_workers(p.worker_limit)) {
            Some(rate) if rate > 0.0 => a as f64 / rate,
            _ => 0.0,
        })
        .sum()
}

/// The share of phase `ph` done, in default seconds (so both parts of a phase in two count); `None` with no total.
pub(crate) fn fraction(p: &Progress, ph: &Phase) -> Option<f64> {
    let total = total_secs(p, ph);
    (total > 0.0).then(|| (done_secs(p, ph) / total).min(1.0))
}

/// Default seconds of what phase `ph` has done.
fn done_secs(p: &Progress, ph: &Phase) -> f64 {
    default_secs(p, ph, [ph.done_of(0), ph.done_of(1)])
}

/// Default seconds of phase `ph`'s whole amount.
fn total_secs(p: &Progress, ph: &Phase) -> f64 {
    default_secs(p, ph, ph.totals.each_ref().map(|t| t.load(Relaxed)))
}

/// A whole phase: a step takes its fixed time; measured work its default time times the factor.
fn scaled_default(p: &Progress, ph: &Phase, factor: f64) -> f64 {
    match ph.spec.unit {
        Unit::Step => STEP.as_secs_f64(),
        _ => total_secs(p, ph) * factor,
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
    // ended phases only: a running phase's counter may run ahead of its time and pull the factor with it
    for ph in p
        .phases
        .iter()
        .filter(|q| q.spec.unit != Unit::Step && q.ended.get().is_some())
    {
        let el = elapsed(ph, now);
        let pred = done_secs(p, ph);
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
