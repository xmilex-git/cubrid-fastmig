//! What the terminal screen shows besides the counters: the run's source and target, how it runs, the devices, the
//! SQL files the warnings point to, and where the report goes. The command fills it once before the display starts.
//!
//! Input: the command's plan. Output: the header, the resources' device line and the notes of the screen
//! ([`crate::render_screen`]).
//! Source: workspace#610 decision 7 (header: source → target; notes: the warnings grouped by the SQL file to run).

/// One SQL file the user runs after the run, with the objects it fixes: a notes row `6 deferred indexes → <file>`.
#[derive(Clone, Debug)]
pub struct SqlNote {
    pub file: String,
    pub objects: usize,
    /// The objects' plural noun (`deferred indexes`, `views`).
    pub noun: String,
}

#[derive(Clone, Debug, Default)]
pub struct ScreenInfo {
    /// `fm114_clean (11.4)`.
    pub source: String,
    /// `fm_new (develop)`.
    pub target: String,
    /// `clone · 16 workers · --check`.
    pub detail: String,
    /// `nvme0n1 → nvme0n1`.
    pub devices: String,
    pub sql: Vec<SqlNote>,
    /// Path of report.json, shown at the foot.
    pub report: String,
}
