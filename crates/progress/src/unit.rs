//! The natural unit a phase counts its work in, and how amounts and rates of it are printed.
//!
//! Input: an amount or a rate. Output: a short human string (`1.2 GiB`, `31 indexes`, `930 k keys/s`).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    /// A short fixed step (open files, write a header): no amount, timed by a default duration only.
    Step,
    Bytes,
    /// A counted item: pages, records, keys, indexes, classes, files (the plural noun).
    Count(&'static str),
}

impl Unit {
    pub fn amount(self, n: u64) -> String {
        match self {
            Unit::Step => String::new(),
            Unit::Bytes => bytes(n as f64),
            Unit::Count(noun) => format!("{} {noun}", count(n as f64)),
        }
    }

    /// `done/total unit`, e.g. `198 MiB/470 MiB` or `31/74 indexes`.
    pub fn fraction(self, done: u64, total: u64) -> String {
        match self {
            Unit::Step => String::new(),
            Unit::Bytes => format!("{}/{}", bytes(done as f64), bytes(total as f64)),
            Unit::Count(noun) => format!("{done}/{total} {noun}"),
        }
    }

    pub fn rate(self, per_sec: f64) -> String {
        match self {
            Unit::Step => String::new(),
            Unit::Bytes => format!("{}/s", bytes(per_sec)),
            Unit::Count(noun) => format!("{} {noun}/s", count(per_sec)),
        }
    }
}

/// Binary multiples: `512 B`, `1.2 GiB`.
pub fn bytes(b: f64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = b;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 || v >= 100.0 {
        format!("{v:.0} {}", UNITS[u])
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

/// Decimal multiples for counts: `930`, `12.5 k`, `3.1 M`.
fn count(n: f64) -> String {
    if n >= 1e6 {
        format!("{:.1} M", n / 1e6)
    } else if n >= 1e4 {
        format!("{:.1} k", n / 1e3)
    } else {
        format!("{n:.0}")
    }
}
