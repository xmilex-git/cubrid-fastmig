//! The display options every long subcommand shares: `--progress auto|tty|plain|none` (default auto),
//! `--progress-interval <seconds>` (plain lines and the run log, default 10), `--color auto|always|never` and
//! `--ascii` (the screens' colors and characters).
//!
//! Input: the parsed flags. Output: a started [`Monitor`] over the command's [`Progress`]; the [`Style`] of a screen.
//! Source: user request relayed in workspace#472; workspace#610 decisions 3 and 4.

use std::sync::Arc;
use std::time::Duration;

use fastmig_progress::{Mode, Monitor, Progress, Stream, Style};

use crate::args::{CliError, Parsed};
use crate::heap_counter;

pub const FLAGS: &[&str] = &["--progress", "--progress-interval", "--color"];
pub const SWITCHES: &[&str] = &["--ascii"];
/// The options of a command that draws no progress (plan, check).
pub const STYLE_FLAGS: &[&str] = &["--color"];

pub const HELP: &str = "  --progress <auto|tty|plain|none>  progress display on stderr (default auto: the in-place screen when stdout and
                                 stderr are terminals, else one line every --progress-interval)
  --progress-interval <seconds>  plain-line interval, also of the progress lines in fastmig/migrate.log (default 10)
  --color <auto|always|never>    colors of the screen (default auto: on a terminal unless NO_COLOR is set)
  --ascii                        draw the screen with +-| and #. (default: Unicode boxes, unless the locale names a
                                 codeset other than UTF-8)";

/// The help lines of `--color` and `--ascii` for plan and check.
pub const STYLE_HELP: &str = "  --color <auto|always|never>    colors of the terminal output (default auto: on a terminal unless NO_COLOR is set)
  --ascii                        draw the terminal output with +-| and #. (default: Unicode boxes, unless the locale
                                 names a codeset other than UTF-8)";

/// The style of a screen written to `stream`.
pub fn style(p: &Parsed, stream: Stream) -> Result<Style, CliError> {
    let arg = p.value("--color");
    Style::detect(arg, p.switch("--ascii"), stream)
        .ok_or_else(|| CliError::Usage(format!("bad --color {}", arg.unwrap_or(""))))
}

fn interval(p: &Parsed) -> Result<Duration, CliError> {
    let s = match p.value("--progress-interval") {
        Some(v) => v
            .parse::<f64>()
            .ok()
            .filter(|s| *s > 0.0 && s.is_finite())
            .ok_or_else(|| CliError::Usage(format!("bad --progress-interval {v}")))?,
        None => 10.0,
    };
    Ok(Duration::from_secs_f64(s))
}

pub fn start(
    p: &Parsed,
    progress: Arc<Progress>,
    memory_limit: Option<u64>,
    temp_disk: Option<u64>,
) -> Result<Monitor, CliError> {
    let mode_arg = p.value("--progress").unwrap_or("auto");
    let mode = Mode::parse(mode_arg)
        .ok_or_else(|| CliError::Usage(format!("bad --progress {mode_arg}")))?;
    Ok(Monitor::start(
        progress,
        mode,
        interval(p)?,
        heap_counter::now_and_peak,
        memory_limit,
        temp_disk.map(|l| (temp_now_and_peak as fastmig_progress::TempProbe, l)),
        style(p, Stream::Stderr)?,
    ))
}

/// The spill bytes on disk now and at most (`fastmig_index::temp_disk::TEMP_DISK`).
fn temp_now_and_peak() -> (u64, u64) {
    fastmig_index::temp_disk::TEMP_DISK.now_and_peak()
}
