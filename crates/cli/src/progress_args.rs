//! The progress options every long subcommand shares: `--progress auto|tty|plain|none` (default auto) and
//! `--progress-interval <seconds>` (plain lines, default 10).
//!
//! Input: the parsed flags. Output: a started [`Monitor`] over the command's [`Progress`].
//! Source: user request relayed in workspace#472.

use std::sync::Arc;
use std::time::Duration;

use fastmig_progress::{Mode, Monitor, Progress};

use crate::args::{CliError, Parsed};
use crate::heap_counter;

pub const FLAGS: &[&str] = &["--progress", "--progress-interval"];

pub const HELP: &str = "\
  --progress <auto|tty|plain|none>  progress display on stderr (default auto: the in-place screen when stdout and
                                 stderr are terminals, else one line every --progress-interval)
  --progress-interval <seconds>  plain-line interval (default 10)";

pub fn start(
    p: &Parsed,
    progress: Arc<Progress>,
    memory_limit: Option<u64>,
) -> Result<Monitor, CliError> {
    let mode_arg = p.value("--progress").unwrap_or("auto");
    let mode = Mode::parse(mode_arg)
        .ok_or_else(|| CliError::Usage(format!("bad --progress {mode_arg}")))?;
    let interval = match p.value("--progress-interval") {
        Some(v) => v
            .parse::<f64>()
            .ok()
            .filter(|s| *s > 0.0 && s.is_finite())
            .ok_or_else(|| CliError::Usage(format!("bad --progress-interval {v}")))?,
        None => 10.0,
    };
    Ok(Monitor::start(
        progress,
        mode,
        Duration::from_secs_f64(interval),
        heap_counter::now_and_peak,
        memory_limit,
    ))
}
