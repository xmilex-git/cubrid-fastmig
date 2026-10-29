//! The progress part of a run's report.json: per phase its amount, time and measured rate, and the run's total
//! time beside the estimate made when the plan's totals were first known (the evidence for better defaults in
//! [`crate::default_rates`]).
//!
//! Input: the finished [`Progress`]. Output: a JSON object as text.

use std::sync::atomic::Ordering::Relaxed;
use std::time::Instant;

use crate::estimate::elapsed;
use crate::meter::Progress;
use crate::unit::Unit;

pub fn json(p: &Progress) -> String {
    let now = Instant::now();
    let phases: Vec<String> = p
        .phases
        .iter()
        .map(|ph| {
            let unit = match ph.spec.unit {
                Unit::Step => "step",
                Unit::Bytes => "bytes",
                Unit::Count(noun) => noun,
            };
            let s = elapsed(ph, now);
            let rate = if ph.spec.unit != Unit::Step && s > 0.0 {
                format!("{:.1}", ph.done() as f64 / s)
            } else {
                "null".into()
            };
            format!(
                "{{\"name\": \"{}\", \"unit\": \"{unit}\", \"total\": {}, \"done\": {}, \"seconds\": {s:.3}, \"rate_per_s\": {rate}}}",
                ph.spec.name,
                ph.total.load(Relaxed),
                ph.done()
            )
        })
        .collect();
    let est = p
        .start_estimate
        .get()
        .map_or("null".into(), |e| format!("{e:.3}"));
    format!(
        "{{\"estimated_total_s_at_start\": {est}, \"actual_total_s\": {:.3}, \"phases\": [{}]}}",
        p.run_started.elapsed().as_secs_f64(),
        phases.join(", ")
    )
}
