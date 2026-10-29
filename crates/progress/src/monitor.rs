//! The one reader thread: every tick it sums the counters, estimates, and draws the screen or prints lines;
//! a phase boundary wakes it early so the plain display prints the boundary at once.
//!
//! Input: the shared [`Progress`], the display mode, the plain-mode interval, a heap probe and the memory limit (`--mem`).
//! Output: the display on stderr. `--progress none` starts no thread.
//! Source: user request relayed in workspace#472 (TTY 200 ms in place, plain every 5 s and at each boundary).

use std::io::{IsTerminal, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::estimate::Estimator;
use crate::meter::Progress;
use crate::render_line::{self, Heap};
use crate::render_screen::Screen;
use crate::unit::Unit;

/// Redraw period of the terminal display.
const TTY_TICK: Duration = Duration::from_millis(200);

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
    ) -> Monitor {
        let stop = Arc::new(AtomicBool::new(false));
        let thread = (mode != Mode::None).then(|| {
            let stop = stop.clone();
            let tick = if mode == Mode::Tty {
                TTY_TICK
            } else {
                plain_interval
            };
            std::thread::spawn(move || {
                let _ = p.waker.set(std::thread::current());
                watch(&p, mode, tick, heap, memory_limit, &stop)
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
    stop: &AtomicBool,
) {
    let mut est = Estimator::default();
    let mut screen = Screen::new();
    // plain mode: boundaries already printed, and when the last running line went out
    let mut ended_shown = vec![false; p.phases.len()];
    let mut began_shown = vec![false; p.phases.len()];
    let mut last_line = Instant::now();
    loop {
        let stopping = stop.load(Relaxed);
        let now = Instant::now();
        let e = est.tick(p, now);
        let heap = Heap {
            now_max: probe(),
            memory_limit,
        };
        match mode {
            Mode::Tty => screen.draw(p, &e, &heap, now),
            Mode::Plain => {
                let mut out = String::new();
                for (i, ph) in p.phases.iter().enumerate() {
                    if ph.ended.get().is_some() && !ended_shown[i] {
                        ended_shown[i] = true;
                        out += &render_line::finished(p, i, now);
                        out += "\n";
                    }
                }
                let cur = p.current();
                // a step announces itself only by its done line, unless it outlasts an interval
                let announce = p.phases.get(cur).is_some_and(|q| q.spec.unit != Unit::Step);
                if cur < p.phases.len()
                    && !stopping
                    && ((announce && !began_shown[cur]) || now.duration_since(last_line) >= tick)
                {
                    began_shown[cur] = true;
                    last_line = now;
                    out += &render_line::running(p, cur, &e, &heap);
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
        std::thread::park_timeout(tick.saturating_sub(now.elapsed()));
    }
}
