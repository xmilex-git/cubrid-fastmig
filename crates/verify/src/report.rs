//! The verification report: one JSON line per defect, then one summary line.
//!
//! Input: the classified defects, the per-database results, the worker count. Output: JSONL text, and the exit code
//! (0 = no migration defect, 1 = at least one; 2, a tool error, is the caller's).
//! Line shapes: `{"kind":"defect","origin":..,"db":..,"check":<1-5>,"check_name":..,"class":..,"index":..,
//! "object":..,"code":..,"at":..,"expected":..,"actual":..}`; `{"kind":"summary",...}` with per-check counts, the
//! migration / existing / resolved counts, rows, indexes, pairs, not-checked indexes, phase times, workers.
//! Source: G3 workspace#483 Q9; G1 workspace#481 (finalize reads the exit code and the report file).

use std::collections::BTreeMap;
use std::fmt::Write;

use crate::classify::{Classified, DefectOrigin};
use crate::run::DbResult;

/// A JSON string literal.
pub fn json_str(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(o, "\\u{:04x}", c as u32);
            }
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

pub fn defect_line(c: &Classified) -> String {
    let d = &c.defect;
    format!(
        "{{\"kind\":\"defect\",\"origin\":{},\"db\":{},\"check\":{},\"check_name\":{},\"class\":{},\"index\":{},\
         \"object\":{},\"code\":{},\"at\":{},\"expected\":{},\"actual\":{}}}",
        json_str(c.origin.name()),
        json_str(c.db),
        d.check.number(),
        json_str(d.check.name()),
        json_str(&d.class),
        json_str(&d.index),
        json_str(&d.object),
        json_str(d.code),
        json_str(&d.at),
        json_str(&d.expected),
        json_str(&d.actual)
    )
}

fn db_summary(r: &DbResult) -> String {
    let phases: Vec<String> = r
        .phases
        .iter()
        .map(|(n, t)| format!("{}:{:.3}", json_str(n), t.as_secs_f64()))
        .collect();
    let nc: Vec<String> = r.not_checked.iter().map(|s| json_str(s)).collect();
    format!(
        "{{\"heaps\":{},\"live_rows\":{},\"indexes\":{},\"index_pairs\":{},\"object_refs\":{},\"lobs\":{},\"foreign_keys\":{},\"index_batches\":{},\"not_checked\":[{}],\"phases_s\":{{{}}}}}",
        r.heaps,
        r.live_rows,
        r.indexes,
        r.index_pairs,
        r.object_refs,
        r.lobs,
        r.foreign_keys,
        r.index_batches,
        nc.join(","),
        phases.join(",")
    )
}

pub fn summary_line(
    all: &[Classified],
    target: &DbResult,
    source: Option<&DbResult>,
    workers: usize,
) -> String {
    let mut by_check: BTreeMap<(&str, u8), usize> = BTreeMap::new();
    let mut by_status: BTreeMap<&str, usize> = BTreeMap::new();
    for c in all {
        *by_check.entry((c.db, c.defect.check.number())).or_default() += 1;
        *by_status.entry(c.origin.name()).or_default() += 1;
    }
    let checks: Vec<String> = by_check
        .iter()
        .map(|((db, n), k)| format!("\"{db}.{n}\":{k}"))
        .collect();
    let count = |s: DefectOrigin| by_status.get(s.name()).copied().unwrap_or(0);
    format!(
        "{{\"kind\":\"summary\",\"migration\":{},\"pre_existing\":{},\"resolved\":{},\"by_check\":{{{}}},\"workers\":{},\
         \"target\":{},\"source\":{}}}",
        count(DefectOrigin::Migration),
        count(DefectOrigin::PreExisting),
        count(DefectOrigin::Resolved),
        checks.join(","),
        workers,
        db_summary(target),
        source.map_or_else(|| "null".into(), db_summary)
    )
}

pub fn exit_code(all: &[Classified]) -> u8 {
    u8::from(all.iter().any(|c| c.origin == DefectOrigin::Migration))
}
