//! The progress part of a run's report.json: per phase its amount, time, measured rate, what it used (CPU seconds,
//! device bytes) and the resource it was bound by; the run's total time beside the estimate made when the plan's
//! totals were first known, and the estimated total at 10, 30 and 60 s and at 20% of the run (the evidence for
//! better defaults in [`crate::default_rates`] and for the speed factors, workspace#560).
//!
//! Input: the finished [`Progress`]. Output: a JSON object as text.
//! The trail points are the monitor's estimates (none with `--progress none`): each is the last one made at or
//! before its time, `null` when the run was shorter or no monitor ran.

use std::sync::atomic::Ordering::Relaxed;
use std::time::Instant;

use crate::estimate::{elapsed, resource_of, used};
use crate::meter::Progress;
use crate::proc_sample::ProcSample;
use crate::resource::Resource;
use crate::unit::Unit;

pub fn json(p: &Progress) -> String {
    let now = Instant::now();
    let sample = ProcSample::now();
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
            let use_ = used(ph, sample).map_or("null".into(), |u| {
                format!(
                    "{{\"cpu_s\": {:.3}, \"read_bytes\": {}, \"write_bytes\": {}}}",
                    u.cpu_s, u.read_bytes, u.write_bytes
                )
            });
            format!(
                "{{\"name\": \"{}\", \"unit\": \"{unit}\", \"total\": {}, \"done\": {}, \"seconds\": {s:.3}, \"rate_per_s\": {rate}, \"used\": {use_}, \"resource\": \"{}\"}}",
                ph.spec.name,
                ph.total.load(Relaxed),
                ph.done(),
                resource_of(ph, now, sample, p.worker_limit).name()
            )
        })
        .collect();
    let observations: Vec<String> = p
        .observations
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(|o| {
            let r = o.used.map_or(Resource::Cpu, |u| {
                Resource::classify(u, o.seconds, p.worker_limit)
            });
            format!(
                "{{\"seconds\": {:.3}, \"predicted_s\": {:.3}, \"resource\": \"{}\"}}",
                o.seconds,
                o.predicted_s,
                r.name()
            )
        })
        .collect();
    let actual = now.duration_since(p.run_started).as_secs_f64();
    let trail = p.trail.lock().unwrap_or_else(|e| e.into_inner());
    let at = |t: f64| {
        trail
            .iter()
            .take_while(|(el, _)| *el <= t)
            .last()
            .filter(|_| t <= actual)
            .map_or("null".into(), |(el, total)| {
                format!("{{\"at_s\": {el:.1}, \"estimated_total_s\": {total:.3}}}")
            })
    };
    let est = p
        .start_estimate
        .get()
        .map_or("null".into(), |e| format!("{e:.3}"));
    format!(
        "{{\"estimated_total_s_at_start\": {est}, \"actual_total_s\": {actual:.3}, \"estimated_total_s_at\": {{\"10s\": {}, \"30s\": {}, \"60s\": {}, \"20%\": {}}}, \"observations\": [{}], \"phases\": [{}]}}",
        at(10.0),
        at(30.0),
        at(60.0),
        at(0.2 * actual),
        observations.join(", "),
        phases.join(", ")
    )
}
