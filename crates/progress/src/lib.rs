//! Progress of a run: the phases it goes through, how far each has come, and its display (an in-place screen on
//! a terminal, one line per interval otherwise).
//!
//! Workers only add to per-thread counters ([`Meter::add`]); one monitor thread reads them, times them, estimates
//! the remaining time and prints. Nothing here knows an on-disk layout.
//! Source: user request relayed in workspace#472 (progress phase/percent, throughput, ETA, heap counter).

pub mod bottleneck;
pub mod clock;
pub mod default_rates;
pub mod estimate;
pub mod meter;
pub mod monitor;
pub mod proc_sample;
pub mod render_line;
pub mod render_screen;
pub mod report;
pub mod storage_kind;
pub mod unit;

pub use bottleneck::{Bottleneck, DeviceThroughput};
pub use default_rates::DefaultRate;
pub use meter::{Meter, PhaseSpec, Progress};
pub use monitor::{HeapProbe, Mode, Monitor, TempProbe};
pub use proc_sample::ProcSample;
pub use storage_kind::StorageKind;
pub use unit::Unit;
