//! The one reader thread: every tick it sums the counters, estimates, and draws the screen or prints a line.
//! Plain mode prints exactly one line per interval (the first at start), naming the phases that ended since
//! the previous line, and the summary at the end; a phase boundary or a warning wakes the thread early so the
//! screen redraws and warnings go out at once.
//!
//! Input: the shared [`Progress`], the display mode, the plain-mode interval, a heap probe and the memory limit (`--mem`).
//! Output: the display on stderr. `--progress none` starts no thread.
//! Source: user request relayed in workspace#472 (TTY 200 ms in place; plain one line every 10 s, not per phase).

use std::io::{IsTerminal, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::estimate::Estimator;
use crate::meter::Progress;
use crate::proc_sample::ProcSample;
use crate::render_line::{self, Heap};
use crate::render_screen::Screen;

/// Redraw period of the terminal display.
const TTY_TICK: Duration = Duration::from_millis(200);
/// Longest gap between two estimates in plain mode (workspace#560: the ETA trail at 10/30/60 s).
const ESTIMATE_TICK: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Tty,
    Plain,
    None,
}

impl Mode {
    /// `auto|tty|plain|none`; `auto` is the screen when both stdout and stderr are terminals, else plain lines
    /// (the screen is drawn on stderr, and output on stdout would tear it).
    pub fn parse(s: &str) -> Option<Mode> {
        match s {
            "auto" => Some(
                if std::io::stdout().is_terminal() && std::io::stderr().is_terminal() {
                    Mode::Tty
                } else {
                    Mode::Plain
                },
            ),
            "tty" => Some(Mode::Tty),
            "plain" => Some(Mode::Plain),
            "none" => Some(Mode::None),
            _ => None,
        }
    }
}

/// Current and maximum heap bytes, `None` when the binary does not count them.
pub type HeapProbe = fn() -> Option<(u64, u64)>;
/// Temp disk bytes now and at most (workspace's spill accounting), shown next to the heap when a limit is set.
pub type TempProbe = fn() -> (u64, u64);

pub struct Monitor {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Monitor {
    pub fn start(
        p: Arc<Progress>,
        mode: Mode,
        plain_interval: Duration,
        heap: HeapProbe,
        memory_limit: Option<u64>,
        temp: Option<(TempProbe, u64)>,
    ) -> Monitor {
        let stop = Arc::new(AtomicBool::new(false));
        let thread = (mode != Mode::None).then(|| {
            let stop = stop.clone();
            let tick = if mode == Mode::Tty {
                TTY_TICK
            } else {
                plain_interval
            };
            *p.warnings.lock().unwrap_or_else(|e| e.into_inner()) = Some(Vec::new());
            std::thread::spawn(move || {
                let _ = p.waker.set(std::thread::current());
                watch(&p, mode, tick, heap, memory_limit, temp, &stop)
            })
        });
        Monitor { stop, thread }
    }

    /// Draws the final state and stops the thread.
    pub fn finish(mut self) {
        self.stop_now();
    }

    fn stop_now(&mut self) {
        if let Some(t) = self.thread.take() {
            self.stop.store(true, Relaxed);
            t.thread().unpark();
            let _ = t.join();
        }
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        self.stop_now();
    }
}

fn watch(
    p: &Progress,
    mode: Mode,
    tick: Duration,
    probe: HeapProbe,
    memory_limit: Option<u64>,
    temp: Option<(TempProbe, u64)>,
    stop: &AtomicBool,
) {
    let mut est = Estimator::default();
    let mut screen = Screen::new();
    // plain mode: phases already named in a line, and when the next line is due (the first at once)
    let mut ended_shown = vec![false; p.phases.len()];
    let mut next_line = Instant::now();
    loop {
        let stopping = stop.load(Relaxed);
        let now = Instant::now();
        let e = est.tick(p, now, ProcSample::now());
        if p.current() < p.phases.len() {
            let el = now.duration_since(p.run_started).as_secs_f64();
            p.trail
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((el, el + e.total_eta));
        }
        let heap = Heap {
            now_max: probe(),
            memory_limit,
            temp: temp.map(|(t, limit)| {
                let (now, max) = t();
                (now, max, limit)
            }),
        };
        let warnings = p.take_warnings(stopping);
        match mode {
            Mode::Tty => {
                if !warnings.is_empty() {
                    screen.print_above(&warnings);
                }
                screen.draw(p, &e, &heap, now);
            }
            Mode::Plain => {
                let mut out = String::new();
                for w in &warnings {
                    out += w;
                    out += "\n";
                }
                let cur = p.current();
                if cur < p.phases.len() && !stopping && now >= next_line {
                    next_line = now + tick;
                    let since: Vec<String> = (0..p.phases.len())
                        .filter(|&i| {
                            p.phases[i].ended.get().is_some()
                                && !std::mem::replace(&mut ended_shown[i], true)
                        })
                        .map(|i| render_line::ended_brief(p, i, now))
                        .collect();
                    out += &render_line::running(p, cur, &e, &heap);
                    if !since.is_empty() {
                        out += &format!(" (since last: {})", since.join(", "));
                    }
                    out += "\n";
                }
                let _ = std::io::stderr().lock().write_all(out.as_bytes());
            }
            Mode::None => {}
        }
        if stopping {
            let _ = writeln!(std::io::stderr(), "{}", render_line::summary(p, &heap));
            break;
        }
        // a boundary or a warning unparks early; plain lines still keep to the interval, the estimate (and its trail
        // in report.json) is still updated every [`ESTIMATE_TICK`]
        let wake = if mode == Mode::Plain {
            next_line.min(now + ESTIMATE_TICK)
        } else {
            now + tick
        };
        std::thread::park_timeout(wake.saturating_duration_since(Instant::now()));
    }
}
