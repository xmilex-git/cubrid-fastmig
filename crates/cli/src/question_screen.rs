//! The question screen of migrate on a terminal: the paths of the run once, then a box with the temp disk and temp
//! mem figures (min, recommended, max, free; the index batches and extra time a temp disk limit costs) and one small
//! input box per limit, the recommended value taken on Enter.
//!
//! Input: the [`Style`] of stderr, the [`PathRow`]s, one [`Question`] per limit (a limit set by an option or the conf
//! is shown, not asked). Output: the answers in bytes, in the order of the questions.
//! The terminal stays in its own (cooked) mode: the cursor is moved into the input box and one line is read; the
//! frame below the paths is redrawn in place after each answer (CUU, EL, ED as the progress screen). An answer that is
//! not `min`, `max` or a size, below min or over free is named in red inside the box and the same box asks again.
//! End of input ends the run with an error before anything is written; Ctrl-C stops it as anywhere else.
//! shortcut: an answer longer than the terminal is wide wraps and the next redraw starts too low, until a user
//! reports it.
//! Source: workspace#612 decisions 1-6 (user, 2026-11-02); the look of workspace#610 (boxes, colors, `--ascii`).

use std::io::{self, BufRead, Write};

use fastmig_progress::panel::{boxed, boxed_whole, fit, side_by_side, wrap};
use fastmig_progress::style::term_size;
use fastmig_progress::{Color, PathRow, Stream, Style, path_panel, unit};

use crate::limits::parse_size;

/// One limit the screen shows and, unless it is already set, asks.
pub struct Question {
    /// `temp disk`, `temp mem`.
    pub name: &'static str,
    pub min: u64,
    pub recommended: u64,
    pub max: u64,
    /// The most the limit may take now: the room on the spill filesystem, the physical memory.
    pub free: u64,
    /// What `free` is, one sentence.
    pub free_means: String,
    /// The index batches and extra time at min, recommended and max (temp disk only).
    pub extra: Option<[String; 3]>,
    /// The value already set and by what (`--temp-mem-size`, `the target conf`): shown, not asked.
    pub preset: Option<(u64, &'static str)>,
}

/// Width from which the input boxes stand side by side.
const SIDE_MIN: usize = 60;
/// Column of the answer inside an input box: the side, a space, the prompt mark and a space.
const ANSWER_COL: usize = 4;

fn b(n: u64) -> String {
    unit::bytes(n as f64)
}

/// The state of one input box.
enum Field {
    Asking(Option<String>),
    Taken(u64, String),
}

/// `min`, `max` or a size, inside min..=free.
fn answer(q: &Question, line: &str) -> Result<u64, String> {
    let v = match line.trim() {
        "" => q.recommended,
        "min" => q.min,
        "max" => q.max,
        s => parse_size(s).ok_or_else(|| format!("{s:?} is not min, max or a size"))?,
    };
    if v < q.min {
        return Err(format!("{} is below min {}", b(v), b(q.min)));
    }
    if v > q.free {
        return Err(format!("{} is over free {}", b(v), b(q.free)));
    }
    Ok(v)
}

/// Asks every question without a preset; the answers (or presets) in bytes.
pub fn ask(st: &Style, paths: &[PathRow], qs: &[Question]) -> io::Result<Vec<u64>> {
    if let Some(q) = qs.iter().find(|q| q.preset.is_none() && q.min > q.free) {
        return Err(io::Error::other(format!(
            "{}: min {} is over free {} ({})",
            q.name,
            b(q.min),
            b(q.free),
            q.free_means
        )));
    }
    let mut err = io::stderr();
    let (cols, _) = term_size(Stream::Stderr);
    let width = cols.saturating_sub(1).max(40);
    let mut text = String::new();
    for l in boxed_whole(
        st,
        "paths of this run (Ctrl-C now stops before anything is written)",
        &path_panel::lines(st, paths, width - 4),
        Color::Blue,
        width,
    ) {
        text += &l;
        text += "\n";
    }
    err.write_all(text.as_bytes())?;
    let mut fields: Vec<Field> = (qs.iter())
        .map(|q| match q.preset {
            Some((v, by)) => Field::Taken(v, format!("set by {by}")),
            None => Field::Asking(None),
        })
        .collect();
    let mut up = 0;
    loop {
        let (lines, inputs) = frame(st, qs, &fields, width);
        let mut out = String::new();
        if up > 0 {
            out += &format!("\x1b[{up}A");
        }
        for l in &lines {
            out += "\r\x1b[2K";
            out += l;
            out += "\n";
        }
        out += "\x1b[J";
        let Some(i) = fields.iter().position(|f| matches!(f, Field::Asking(_))) else {
            err.write_all(out.as_bytes())?;
            err.flush()?;
            break;
        };
        let (row, col) = inputs[i];
        out += &format!("\x1b[{}A\r\x1b[{col}C", lines.len() - row);
        err.write_all(out.as_bytes())?;
        err.flush()?;
        let mut line = String::new();
        if io::stdin().lock().read_line(&mut line)? == 0 {
            // below the frame before the error is printed
            err.write_all(format!("\x1b[{}B\n", lines.len() - row).as_bytes())?;
            return Err(io::Error::other(format!(
                "{}: no answer (end of input)",
                qs[i].name
            )));
        }
        // the Enter took the cursor to the row below the answer
        up = row + 1;
        fields[i] = match answer(&qs[i], &line) {
            Ok(v) => Field::Taken(v, "taken".into()),
            Err(e) => Field::Asking(Some(e)),
        };
    }
    Ok(fields
        .iter()
        .map(|f| match f {
            Field::Taken(v, _) => *v,
            Field::Asking(_) => 0,
        })
        .collect())
}

/// The frame's lines and, per question, the (row, column) its answer is typed at.
fn frame(
    st: &Style,
    qs: &[Question],
    fields: &[Field],
    width: usize,
) -> (Vec<String>, Vec<(usize, usize)>) {
    let inner = width - 4;
    let col = |s: &str| format!("{s:<14}");
    let mut table = vec![st.paint(
        Color::Gray,
        &format!(
            "{:<10}{}{}{}{}",
            "",
            col("min"),
            col("recommended"),
            col("max"),
            col("free")
        ),
    )];
    for q in qs {
        table.push(format!(
            "{:<10}{}{}{}{}",
            q.name,
            col(&b(q.min)),
            st.strong(Color::Green, &col(&b(q.recommended))),
            col(&b(q.max)),
            col(&b(q.free))
        ));
        if let Some([lo, rec, hi]) = &q.extra {
            table.push(st.paint(
                Color::Gray,
                &format!("{:<10}{}{}{}", "  batches", col(lo), col(rec), col(hi)),
            ));
        }
    }
    for q in qs {
        table.extend(
            wrap(&format!("{}: {}", q.name, q.free_means), inner)
                .iter()
                .map(|l| st.paint(Color::Gray, l)),
        );
    }
    let mut lines = boxed(
        st,
        "temp disk and memory of this run",
        &table,
        Color::Magenta,
        width,
    );
    let side = width >= SIDE_MIN;
    let box_w = if side {
        (width - (qs.len() - 1)) / qs.len()
    } else {
        width
    };
    let boxes: Vec<Vec<String>> = (qs.iter().zip(fields))
        .map(|(q, f)| input_box(st, q, f, box_w))
        .collect();
    let mut inputs = Vec::new();
    if side {
        let top = lines.len();
        for i in 0..qs.len() {
            inputs.push((top + 1, i * (box_w + 1) + ANSWER_COL));
        }
        let joined = boxes.into_iter().reduce(side_by_side).unwrap_or_default();
        lines.extend(joined);
    } else {
        for bx in boxes {
            inputs.push((lines.len() + 1, ANSWER_COL));
            lines.extend(bx);
        }
    }
    if fields.iter().all(|f| matches!(f, Field::Taken(..))) {
        return (lines, inputs);
    }
    let dot = st.glyphs.dot;
    lines.push(st.paint(
        Color::Gray,
        fit(
            &format!(" Enter: recommended {dot} min, max, 900M, 2G {dot} Ctrl-C stops before any write"),
            width,
        )
        .trim_end(),
    ));
    (lines, inputs)
}

/// The small box of one limit: the answer row (the prompt and the Enter hint, or the value taken) and a status row.
fn input_box(st: &Style, q: &Question, f: &Field, w: usize) -> Vec<String> {
    let inner = w.max(8) - 4;
    let mark = if st.unicode { "›" } else { ">" };
    let (answer_row, status, color) = match f {
        Field::Asking(e) => {
            let hint = format!("Enter: {}", b(q.recommended));
            let gap = inner.saturating_sub(2 + hint.chars().count());
            let status = match e {
                Some(e) => st.paint(Color::Red, e),
                None => st.paint(Color::Gray, "min, max or a size"),
            };
            (
                format!(
                    "{} {}{}",
                    st.strong(Color::Cyan, mark),
                    " ".repeat(gap),
                    st.paint(Color::Gray, &hint)
                ),
                status,
                if e.is_some() { Color::Red } else { Color::Cyan },
            )
        }
        Field::Taken(v, by) => (
            format!(
                "{} {}",
                st.paint(Color::Green, st.glyphs.done),
                st.strong(Color::Green, &b(*v))
            ),
            st.paint(Color::Gray, by),
            Color::Green,
        ),
    };
    boxed(st, q.name, &[answer_row, status], color, w)
}
