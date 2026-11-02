//! The paths panel: where each kind of file of the run is read from and written to, by absolute path, with how the
//! target gets it (clone, copy, new, none).
//!
//! Input: the [`PathRow`]s, a width, a [`Style`]. Output: one line per row when `label method source → target` fits the
//! width, else two (the target under the source); a path is never cut (the user copies it), so a line may still be
//! wider than the width: [`fits`] tells, and a screen redrawn in place folds the panel then. [`log_lines`] gives the
//! same rows for the run log.
//! Source: workspace#612 decision 6 (paths shown whole before the first write, in plan, progress and the end box).

use crate::panel::{visible_width, wrap};
use crate::screen_info::PathRow;
use crate::style::{Color, Style};

/// The legend under the rows: the methods that are not a copy.
pub const LEGEND: &str =
    "clone: shares blocks with the source · new: written by the run · none: not carried over";

fn method_color(m: &str) -> Color {
    match m {
        "clone" => Color::Green,
        "copy" => Color::Cyan,
        "none" => Color::Gray,
        _ => Color::Yellow,
    }
}

/// The rows as lines `inner` columns wide at most where the paths allow it.
pub fn lines(st: &Style, rows: &[PathRow], inner: usize) -> Vec<String> {
    let label_w = rows
        .iter()
        .map(|r| r.label.chars().count())
        .max()
        .unwrap_or(0);
    let lead = |r: &PathRow| {
        format!(
            "{:<label_w$} {}",
            r.label,
            st.paint(method_color(r.method), &format!("{:<7}", r.method))
        )
    };
    let none = st.paint(Color::Gray, "(none)");
    let mut out = Vec::new();
    for r in rows {
        let source = r.source.as_deref().map_or(none.clone(), str::to_string);
        let to = format!("{} {}", st.glyphs.to, st.strong(Color::Bold, &r.target));
        let one = format!("{} {source} {to}", lead(r));
        if visible_width(&one) <= inner {
            out.push(one);
        } else {
            out.push(format!("{} {source}", lead(r)));
            out.push(format!("{} {to}", " ".repeat(label_w + 8)));
        }
    }
    let legend = if st.unicode {
        LEGEND.to_string()
    } else {
        LEGEND.replace('·', "-")
    };
    out.extend(
        wrap(&legend, inner)
            .iter()
            .map(|l| st.paint(Color::Gray, l)),
    );
    out
}

/// Every line of [`lines`] is at most `inner` columns wide.
pub fn fits(st: &Style, rows: &[PathRow], inner: usize) -> bool {
    lines(st, rows, inner)
        .iter()
        .all(|l| visible_width(l) <= inner)
}

/// `paths: <label> <method> <source> -> <target>`, one per row.
pub fn log_lines(rows: &[PathRow]) -> Vec<String> {
    rows.iter()
        .map(|r| {
            format!(
                "paths: {} {} {} -> {}",
                r.label,
                r.method,
                r.source.as_deref().unwrap_or("(none)"),
                r.target
            )
        })
        .collect()
}
