//! The terminal display: a block of lines on stderr redrawn in place each tick — the run's ETA and heap, then
//! one line per phase (done, running with a bar, waiting).
//!
//! Input: the progress state, this tick's estimate, the heap reading. Output: ANSI cursor-up and erase-line
//! sequences plus the lines, each cut to the terminal width. The terminal's modes and cursor are never changed,
//! so an interrupted run (Ctrl-C) leaves nothing to restore; the last frame stays on screen.
//! A warning is written above the frame: the frame is erased, the warning printed, the frame redrawn below it.
//! Source: ECMA-48 CUU (`ESC [ n A`), EL (`ESC [ 2 K`) and ED (`ESC [ J`); Linux `TIOCGWINSZ` (asm-generic/ioctls.h 0x5413).

use std::io::Write;
use std::sync::atomic::Ordering::Relaxed;
use std::time::Instant;

use crate::clock;
use crate::estimate::{Estimate, elapsed};
use crate::meter::Progress;
use crate::render_line::{Heap, amount, finished_amount, secs};
use crate::unit::Unit;

pub struct Screen {
    /// Lines drawn by the previous frame (the cursor sits below them).
    drawn: usize,
}

impl Screen {
    pub fn new() -> Screen {
        Screen { drawn: 0 }
    }

    /// Erases the drawn frame, writes `lines` where it stood (they scroll up with the output), and leaves the
    /// next [`Screen::draw`] to redraw the frame below them.
    pub fn print_above(&mut self, lines: &[String]) {
        let mut out = String::new();
        if self.drawn > 0 {
            out += &format!("\x1b[{}A\r\x1b[J", self.drawn);
        }
        for l in lines {
            out += l;
            out += "\n";
        }
        self.drawn = 0;
        let mut err = std::io::stderr().lock();
        let _ = err.write_all(out.as_bytes());
        let _ = err.flush();
    }

    pub fn draw(&mut self, p: &Progress, e: &Estimate, heap: &Heap, now: Instant) {
        let width = term_width().saturating_sub(1).max(20);
        let lines = frame(p, e, heap, now, width);
        let mut out = String::new();
        if self.drawn > 0 {
            out += &format!("\x1b[{}A", self.drawn);
        }
        for l in &lines {
            out += "\r\x1b[2K";
            out.extend(l.trim_end().chars().take(width));
            out += "\n";
        }
        self.drawn = lines.len();
        let mut err = std::io::stderr().lock();
        let _ = err.write_all(out.as_bytes());
        let _ = err.flush();
    }
}

impl Default for Screen {
    fn default() -> Self {
        Screen::new()
    }
}

fn frame(p: &Progress, e: &Estimate, heap: &Heap, now: Instant, width: usize) -> Vec<String> {
    let run = now.duration_since(p.run_started).as_secs_f64();
    let over = p.current() >= p.phases.len();
    let mut lines = vec![if over {
        format!(
            "{}  finished in {}  worker limit {}",
            p.title,
            secs(run),
            p.worker_limit
        )
    } else {
        format!(
            "{}  elapsed {}  ETA ~{}  done ~{}  worker limit {}",
            p.title,
            clock::duration(run),
            clock::duration(e.total_eta),
            clock::local_hhmm(e.total_eta),
            p.worker_limit
        )
    }];
    lines.push(heap.text());
    let name_w = p
        .phases
        .iter()
        .map(|q| q.spec.name.len())
        .max()
        .unwrap_or(0);
    for (i, ph) in p.phases.iter().enumerate() {
        let head = format!("{:>2}/{} {:<name_w$}", i + 1, p.phases.len(), ph.spec.name);
        let line = if ph.ended.get().is_some() {
            format!(
                "[x] {head}  {:>8}  {}",
                secs(elapsed(ph, now)),
                finished_amount(ph, now)
            )
        } else if i == p.current() {
            let total = ph.total.load(Relaxed);
            let bar = if ph.spec.unit != Unit::Step && total > 0 {
                // what is left after the name, the amount and the ETA (about 60 columns)
                let bar_w = width.saturating_sub(head.len() + 64).clamp(10, 40);
                let fill = (ph.done().min(total) as f64 / total as f64 * bar_w as f64) as usize;
                format!("[{}{}] ", "#".repeat(fill), ".".repeat(bar_w - fill))
            } else {
                String::new()
            };
            format!(
                "[>] {head}  {bar}{}ETA {}",
                amount(ph, e.rate),
                clock::duration(e.phase_eta)
            )
        } else {
            format!("[ ] {head}")
        };
        lines.push(line);
    }
    lines
}

/// Columns of the terminal on stderr; 100 when it cannot be read.
fn term_width() -> usize {
    #[repr(C)]
    #[derive(Default)]
    struct Winsize {
        row: u16,
        col: u16,
        xpixel: u16,
        ypixel: u16,
    }
    unsafe extern "C" {
        fn ioctl(fd: i32, request: u64, ...) -> i32;
    }
    const TIOCGWINSZ: u64 = 0x5413;
    let mut ws = Winsize::default();
    // SAFETY: TIOCGWINSZ writes one `struct winsize` into `ws`.
    let rc = unsafe { ioctl(2, TIOCGWINSZ, &mut ws as *mut Winsize) };
    if rc == 0 && ws.col > 0 {
        ws.col as usize
    } else {
        100
    }
}
