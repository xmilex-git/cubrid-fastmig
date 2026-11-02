//! The terminal display: boxes on stderr redrawn in place each tick — a header (source → target, the whole run's
//! bar, ETA and end time), resources (memory and temp disk gauges with their peaks, the running phase's bottleneck,
//! the devices), notes (the warnings grouped by the SQL file that fixes them) and phases (a bar, time, amount and
//! rate per phase).
//!
//! Input: the progress state, this tick's estimate, the heap reading, the warnings so far, the [`Style`]. Output: ANSI
//! cursor-up and erase sequences plus the lines, none wider than the terminal less one column. The terminal's modes
//! are never changed and no alternate screen is used, so an interrupted run (Ctrl-C, `kill -9`, a dropped SSH link)
//! leaves nothing to restore; the last frame stays in the scrollback.
//! Fitting: below 100 columns resources and notes are stacked; a phase row drops its rate, then its amount, then
//! narrows its bar; a frame taller than the terminal drops the foot, then merges resources and notes into one box,
//! then folds the finished phases into one row, then leaves resources and notes out.
//! shortcut: a terminal narrowed while drawing wraps the previous frame and the next redraw starts too low, until a
//! user reports it.
//! Source: workspace#610 decisions 2-7; ECMA-48 CUU (`ESC [ n A`), EL (`ESC [ 2 K`), ED (`ESC [ J`).

use std::io::Write;
use std::time::Instant;

use crate::big_text;
use crate::bottleneck::Bottleneck;
use crate::clock;
use crate::estimate::{Estimate, classified, elapsed, fraction, used};
use crate::meter::{Phase, Progress};
use crate::panel::{bar, boxed, cut_middle, fit, gauge, side_by_side};
use crate::proc_sample::ProcSample;
use crate::render_line::{Heap, secs};
use crate::style::{Color, Stream, Style, term_size};
use crate::unit::Unit;

/// Terminal size from which the logo is shown (decision 5).
const LOGO_MIN: (usize, usize) = (100, 40);
/// Width from which resources and notes stand side by side.
const SIDE_MIN: usize = 100;
/// Rows inside the resources and notes boxes.
const PANEL_ROWS: usize = 4;

pub struct Screen {
    style: Style,
    /// Lines drawn by the previous frame (the cursor sits below them).
    drawn: usize,
    /// Every warning so far, in full (the notes box summarizes them; the run log has them).
    warnings: Vec<String>,
}

impl Screen {
    pub fn new(style: Style) -> Screen {
        Screen {
            style,
            drawn: 0,
            warnings: Vec::new(),
        }
    }

    pub fn note(&mut self, warnings: Vec<String>) {
        self.warnings.extend(warnings);
    }

    pub fn draw(
        &mut self,
        p: &Progress,
        e: &Estimate,
        heap: &Heap,
        now: Instant,
        sample: Option<ProcSample>,
    ) {
        let (cols, rows) = term_size(Stream::Stderr);
        let f = Frame {
            st: &self.style,
            p,
            e,
            heap,
            now,
            sample,
            warnings: &self.warnings,
            width: cols.saturating_sub(1).max(40),
            logo: self.style.unicode && cols >= LOGO_MIN.0 && rows >= LOGO_MIN.1,
        };
        let lines = f.lines(rows.saturating_sub(1).max(8));
        let mut out = String::new();
        if self.drawn > 0 {
            out += &format!("\x1b[{}A", self.drawn);
        }
        for l in &lines {
            out += "\r\x1b[2K";
            out += l;
            out += "\n";
        }
        out += "\x1b[J";
        self.drawn = lines.len();
        let mut err = std::io::stderr().lock();
        let _ = err.write_all(out.as_bytes());
        let _ = err.flush();
    }
}

struct Frame<'a> {
    st: &'a Style,
    p: &'a Progress,
    e: &'a Estimate,
    heap: &'a Heap,
    now: Instant,
    sample: Option<ProcSample>,
    warnings: &'a [String],
    width: usize,
    logo: bool,
}

impl Frame<'_> {
    /// The frame in at most `max_rows` lines.
    fn lines(&self, max_rows: usize) -> Vec<String> {
        let head = self.header();
        let mut foot = vec![self.foot()];
        let mut middle = self.resources_and_notes();
        let mut folded = 0;
        let mut phases = self.phases(folded);
        let total = |h: &[String], m: &[String], ph: &[String], f: &[String]| {
            h.len() + m.len() + ph.len() + f.len()
        };
        if total(&head, &middle, &phases, &foot) > max_rows {
            foot.clear();
        }
        if total(&head, &middle, &phases, &foot) > max_rows {
            middle = self.merged_panel();
        }
        while total(&head, &middle, &phases, &foot) > max_rows && folded < self.done_count() {
            folded += 1;
            phases = self.phases(folded);
        }
        if total(&head, &middle, &phases, &foot) > max_rows {
            middle.clear();
        }
        [head, middle, phases, foot].concat()
    }

    fn done_count(&self) -> usize {
        (self.p.phases.iter())
            .take_while(|ph| ph.ended.get().is_some())
            .count()
    }

    fn info_or<'b>(&'b self, f: impl Fn(&'b crate::screen_info::ScreenInfo) -> &'b str) -> &'b str {
        self.p.screen.get().map_or("", f)
    }

    fn header(&self) -> Vec<String> {
        let st = self.st;
        let p = self.p;
        let mut lines = Vec::new();
        if self.logo {
            let [a, b, c] = big_text::rows("cubrid-fastmig");
            lines.push(st.strong(Color::Cyan, &format!(" {a}")));
            lines.push(st.strong(Color::Cyan, &format!(" {b}")));
            lines.push(st.strong(Color::Blue, &format!(" {c}")));
            lines.push(String::new());
        }
        let (src, tgt) = (self.info_or(|i| &i.source), self.info_or(|i| &i.target));
        let detail = match self.info_or(|i| &i.detail) {
            "" => format!("{} workers", p.worker_limit),
            d => d.to_string(),
        };
        if !src.is_empty() {
            lines.push(format!(
                "{}  {}  {}     {detail}",
                st.strong(Color::Bold, src),
                st.paint(Color::Yellow, st.glyphs.arrow),
                st.strong(Color::Bold, tgt)
            ));
        } else {
            lines.push(detail);
        }
        let run = self.now.duration_since(p.run_started).as_secs_f64();
        let bar_w = 30.min(self.width.saturating_sub(60)).max(10);
        let line = if p.current() >= p.phases.len() {
            let est = p.start_estimate.get().map_or(String::new(), |e| {
                format!("   estimated at start ~{}", secs(*e))
            });
            format!(
                "finished in {}{est}   {} {}",
                st.strong(Color::Green, &secs(run)),
                bar(st, 1.0, bar_w, Color::Green),
                st.strong(Color::Green, "100%")
            )
        } else {
            let f = run / (run + self.e.total_eta).max(1e-9);
            format!(
                "elapsed {}   ETA {}   done at {}   {} {}",
                st.strong(Color::Bold, &clock::duration(run)),
                st.strong(
                    Color::Green,
                    &format!("~{}", clock::duration(self.e.total_eta))
                ),
                st.strong(Color::Bold, &clock::local_hhmm(self.e.total_eta)),
                bar(st, f, bar_w, Color::Green),
                st.strong(Color::Bold, &format!("{:.0}%", f * 100.0))
            )
        };
        lines.push(line);
        boxed(
            st,
            &format!("cubrid-fastmig {}", p.title),
            &lines,
            Color::Cyan,
            self.width,
        )
    }

    /// The running phase, or the last one once all ended.
    fn current_phase(&self) -> Option<&Phase> {
        let p = self.p;
        p.phases.get(p.current()).or(p.phases.last())
    }

    fn gauges(&self, inner: usize) -> Vec<String> {
        let st = self.st;
        let gib = |b: u64| b as f64 / (1u64 << 30) as f64;
        // label 5, numbers about 24
        let w = inner.saturating_sub(31).clamp(4, 22);
        let mut out = Vec::new();
        out.push(match (self.heap.now_max, self.heap.memory_limit) {
            (Some((now, max)), Some(limit)) => format!(
                "mem   {} {:>5.1}/{:.1} GiB  {}",
                gauge(st, gib(now), gib(max), gib(limit), w, Color::Magenta),
                gib(now),
                gib(limit),
                st.paint(Color::Gray, &format!("peak {:.1}", gib(max)))
            ),
            _ => format!("mem   {}", self.heap.text()),
        });
        out.push(match self.heap.temp {
            Some((now, max, limit)) => format!(
                "temp  {} {:>5.1}/{:.1} GiB  {}",
                gauge(st, gib(now), gib(max), gib(limit), w, Color::Yellow),
                gib(now),
                gib(limit),
                st.paint(Color::Gray, &format!("peak {:.1}", gib(max)))
            ),
            None => format!("temp  {}", st.paint(Color::Gray, "no limit")),
        });
        out
    }

    /// `cpu (93 cpu·s in 7.5 s)`, the running phase's bottleneck; `(prior)` before it is classified.
    fn bottleneck(&self) -> String {
        let st = self.st;
        let Some(ph) = self.current_phase() else {
            return String::new();
        };
        let shown = classified(ph, self.now, self.sample, self.p.worker_limit);
        let name = shown.unwrap_or(ph.spec.prior).name();
        let how = match (shown, used(ph, self.sample)) {
            (Some(_), Some(u)) => format!(
                " ({:.0} cpu{}s in {:.1} s)",
                u.cpu_s,
                st.glyphs.dot,
                elapsed(ph, self.now)
            ),
            _ => " (prior)".into(),
        };
        let color = match shown {
            Some(Bottleneck::Cpu) | None => Color::Green,
            Some(_) => Color::Yellow,
        };
        format!(
            "bound {}{}  {}",
            st.paint(color, name),
            st.paint(Color::Gray, &how),
            st.paint(Color::Gray, ph.spec.name)
        )
    }

    fn resource_lines(&self, inner: usize) -> Vec<String> {
        let mut out = self.gauges(inner);
        out.push(self.bottleneck());
        out.push(format!(
            "disk  {}",
            self.st.paint(Color::Gray, self.info_or(|i| &i.devices))
        ));
        out
    }

    /// The notes rows: one per SQL file with objects, then the other warnings, cut to `rows` with a last row naming
    /// what was left out and the run log.
    fn note_lines(&self, rows: usize) -> Vec<String> {
        let st = self.st;
        let sql = self.p.screen.get().map_or(&[][..], |i| &i.sql[..]);
        let mut out: Vec<String> = (sql.iter())
            .filter(|s| s.objects > 0)
            .map(|s| {
                format!(
                    "{} {} {} {} {}",
                    st.paint(Color::Yellow, st.glyphs.warning),
                    s.objects,
                    s.noun,
                    st.glyphs.to,
                    st.strong(Color::Yellow, &s.file)
                )
            })
            .collect();
        let others: Vec<&str> = (self.warnings.iter())
            .filter(|w| !sql.iter().any(|s| w.contains(&s.file)))
            .map(|w| w.strip_prefix("warning: ").unwrap_or(w))
            .collect();
        // the file's name: its whole path is at the foot
        let log = (self.p.log_path())
            .and_then(|l| Some(l.file_name()?.to_string_lossy().into_owned()))
            .unwrap_or("the run log".into());
        let room = rows.saturating_sub(out.len() + 1);
        for w in others.iter().take(room) {
            out.push(st.paint(Color::Gray, &format!("{} {w}", st.glyphs.dot)));
        }
        let left = others.len().saturating_sub(room);
        let last = if left > 0 {
            format!("+{left} more, full text in {log}")
        } else {
            format!("full text in {log}")
        };
        out.push(st.paint(Color::Gray, &last));
        out
    }

    fn notes_title(&self) -> String {
        format!("notes ({})", self.warnings.len())
    }

    fn resources_and_notes(&self) -> Vec<String> {
        let st = self.st;
        if self.width >= SIDE_MIN {
            let half = (self.width - 1) / 2;
            let other = self.width - 1 - half;
            let res = self.resource_lines(half - 4);
            let notes = self.note_lines(PANEL_ROWS);
            let rows = PANEL_ROWS.max(res.len()).max(notes.len());
            let pad = |mut v: Vec<String>| {
                v.resize(rows, String::new());
                v
            };
            side_by_side(
                boxed(st, "resources", &pad(res), Color::Magenta, half),
                boxed(st, &self.notes_title(), &pad(notes), Color::Yellow, other),
            )
        } else {
            let mut out = boxed(
                st,
                "resources",
                &self.resource_lines(self.width - 4),
                Color::Magenta,
                self.width,
            );
            out.extend(boxed(
                st,
                &self.notes_title(),
                &self.note_lines(PANEL_ROWS),
                Color::Yellow,
                self.width,
            ));
            out
        }
    }

    /// Resources and notes in one three-row box, for a short terminal.
    fn merged_panel(&self) -> Vec<String> {
        let st = self.st;
        let lines = vec![
            self.gauges((self.width - 4) / 2).join("  "),
            self.bottleneck(),
            self.note_lines(2).join("  "),
        ];
        boxed(
            st,
            &format!("resources {} {}", st.glyphs.dot, self.notes_title()),
            &lines,
            Color::Magenta,
            self.width,
        )
    }

    /// The phases box; the first `folded` (finished) phases make one row.
    fn phases(&self, folded: usize) -> Vec<String> {
        let st = self.st;
        let p = self.p;
        let inner = self.width - 4;
        let name_w = (p.phases.iter())
            .map(|q| q.spec.name.len())
            .max()
            .unwrap_or(0);
        let rows: Vec<Row> = (p.phases.iter().enumerate())
            .map(|(i, ph)| self.row(i, ph))
            .collect();
        // marker, number, name; then the bar, the share (4) and the time (8)
        let fixed = 1 + 1 + 2 + 1 + name_w + 1;
        let left = inner.saturating_sub(fixed + 1 + 4 + 2 + 8);
        let amount_w = rows
            .iter()
            .map(|r| r.amount.chars().count())
            .max()
            .unwrap_or(0);
        let rate_w = rows
            .iter()
            .map(|r| r.rate.chars().count())
            .max()
            .unwrap_or(0);
        const BAR: (usize, usize) = (6, 26);
        // the amount column: at most AMOUNT.1 wide (a phase in parts is cut), left out under AMOUNT.0
        const AMOUNT: (usize, usize) = (12, 28);
        let amount_w = amount_w.min(AMOUNT.1);
        // the rate goes first, then the amount, then the bar narrows (workspace#610)
        let show_rate = left >= BAR.1 + 2 + amount_w + 2 + rate_w;
        let amount_w = amount_w.min(left.saturating_sub(BAR.1 + 2));
        let show_amount = show_rate || amount_w >= AMOUNT.0;
        let bar_w = if show_amount {
            BAR.1
        } else {
            left.clamp(BAR.0, BAR.1)
        };
        let mut out = Vec::new();
        if folded > 0 {
            let s: f64 = (p.phases[..folded].iter())
                .map(|ph| elapsed(ph, self.now))
                .sum();
            out.push(format!(
                "{} {} {}",
                st.paint(Color::Green, st.glyphs.done),
                st.paint(Color::Gray, &format!("{:>2}", format!("1-{folded}"))),
                st.paint(Color::Green, &format!("done in {}", secs(s)))
            ));
        }
        for (i, r) in rows.iter().enumerate().skip(folded) {
            let (mark, color) = match r.state {
                State::Done => (st.glyphs.done, Color::Green),
                State::Running => (st.glyphs.running, Color::Cyan),
                State::Waiting => (st.glyphs.waiting, Color::Gray),
            };
            let b = match (r.state, r.fraction) {
                (State::Waiting, _) | (State::Running, None) => {
                    st.paint(Color::Gray, &st.glyphs.dot.repeat(bar_w))
                }
                (_, Some(f)) => bar(st, f, bar_w, color),
                (State::Done, None) => bar(st, 1.0, bar_w, color),
            };
            let pct = match (r.state, r.fraction) {
                (State::Waiting, _) | (State::Running, None) => "    ".to_string(),
                (State::Done, _) => " ok ".to_string(),
                (_, Some(f)) => format!("{:>3}%", (f * 100.0) as u32),
            };
            let mut line = format!(
                "{} {} {} {} {}  {:>8}",
                st.paint(color, mark),
                st.paint(Color::Gray, &format!("{:>2}", i + 1)),
                st.paint(
                    if r.state == State::Waiting {
                        Color::Gray
                    } else {
                        color
                    },
                    &format!("{:<name_w$}", p.phases[i].spec.name)
                ),
                b,
                pct,
                r.time
            );
            if show_amount {
                let a: String = r.amount.chars().take(amount_w).collect();
                line += &format!("  {a:<amount_w$}");
            }
            if show_rate {
                line += &format!("  {}", st.paint(Color::Gray, &r.rate));
            }
            out.push(line);
        }
        boxed(st, "phases", &out, Color::Blue, self.width)
    }

    fn row(&self, i: usize, ph: &Phase) -> Row {
        let p = self.p;
        let step = ph.spec.unit == Unit::Step;
        if ph.ended.get().is_some() {
            let el = elapsed(ph, self.now);
            let done = ph.done();
            return Row {
                state: State::Done,
                fraction: Some(1.0),
                time: secs(el),
                amount: if step {
                    String::new()
                } else {
                    ph.spec.unit.amount(done)
                },
                rate: if step || el <= 0.0 {
                    String::new()
                } else {
                    ph.spec.unit.rate(done as f64 / el)
                },
            };
        }
        if i != p.current() {
            return Row {
                state: State::Waiting,
                fraction: None,
                time: String::new(),
                amount: String::new(),
                rate: String::new(),
            };
        }
        // the parts under way (begun, not complete), else the last begun one: a phase in four parts fits a column
        let shown = |k: usize| {
            let total = ph.totals[k].load(std::sync::atomic::Ordering::Relaxed);
            let done = ph.done_of(k);
            (done > 0, done < total)
        };
        let n = ph.totals.len();
        let mut ks: Vec<usize> = (0..n).filter(|&k| shown(k) == (true, true)).collect();
        if ks.is_empty() {
            ks.push((0..n).rev().find(|&k| shown(k).0).unwrap_or(0));
        }
        let units: Vec<Unit> = ph.spec.all_parts().map(|(u, _)| u).collect();
        let parts: Vec<String> = (ks.into_iter())
            .map(|k| {
                let total = ph.totals[k].load(std::sync::atomic::Ordering::Relaxed);
                let done = ph.done_of(k);
                if total == 0 {
                    units[k].amount(done)
                } else {
                    units[k].short_fraction(done.min(total), total)
                }
            })
            .collect();
        Row {
            state: State::Running,
            fraction: if step { None } else { fraction(p, ph) },
            time: format!("ETA {}", clock::duration(self.e.phase_eta)),
            amount: if step {
                String::new()
            } else {
                parts.join(&format!(" {} ", self.st.glyphs.dot))
            },
            rate: match self.e.rate {
                Some(r) if !step && ph.spec.more.is_empty() => ph.spec.unit.rate(r),
                _ => String::new(),
            },
        }
    }

    /// The run log's path (report.json is beside it), or the report's before the log is open.
    fn foot(&self) -> String {
        let st = self.st;
        let (what, path) = match self.p.log_path() {
            Some(l) => ("log (report.json beside it)", l.display().to_string()),
            None => ("report", self.info_or(|i| &i.report).to_string()),
        };
        let head = format!(" Ctrl-C stops {} {what}: ", st.glyphs.dot);
        let room = self.width.saturating_sub(head.chars().count());
        st.paint(
            Color::Gray,
            &fit(
                &format!("{head}{}", cut_middle(st, &path, room)),
                self.width,
            ),
        )
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Done,
    Running,
    Waiting,
}

/// One phase row's texts.
struct Row {
    state: State,
    fraction: Option<f64>,
    time: String,
    amount: String,
    rate: String,
}
