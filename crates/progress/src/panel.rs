//! The pieces the terminal screens are built of: a titled box of lines, two boxes side by side, a bar, a gauge, and
//! cutting a painted line to a width.
//!
//! Input: lines that may hold SGR color sequences, a width in columns, a [`Style`]. Output: lines whose visible width
//! is exactly the width asked for (a box) or at most it (a cut line). Every character drawn here is one column wide.
//! Source: workspace#610 decisions 4 and 7 (btop-style rounded boxes, `█░` bars, ANSI written directly).

use crate::style::{Color, Style};

/// Columns `s` takes on screen: its characters less the SGR sequences (`ESC [ ... m`).
pub fn visible_width(s: &str) -> usize {
    let mut n = 0;
    let mut esc = false;
    for c in s.chars() {
        match (esc, c) {
            (false, '\x1b') => esc = true,
            (true, 'm') => esc = false,
            (true, _) => {}
            (false, _) => n += 1,
        }
    }
    n
}

/// `s` cut to `w` visible columns (the SGR sequences kept, a reset added after a cut), then padded with spaces to `w`.
pub fn fit(s: &str, w: usize) -> String {
    let mut out = String::new();
    let mut n = 0;
    let mut esc = false;
    let mut cut = false;
    for c in s.chars() {
        match (esc, c) {
            (false, '\x1b') => {
                esc = true;
                out.push(c);
            }
            (true, _) => {
                esc = c != 'm';
                out.push(c);
            }
            (false, _) if n < w => {
                n += 1;
                out.push(c);
            }
            (false, _) => {
                cut = true;
                break;
            }
        }
    }
    if cut && out.contains('\x1b') {
        out += "\x1b[0m";
    }
    out.extend(std::iter::repeat_n(' ', w - n));
    out
}

/// `s` cut in the middle to `w` columns with `…` (`~` in ASCII): for paths, whose both ends matter. Plain text only.
pub fn cut_middle(st: &Style, s: &str, w: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= w || w < 3 {
        return s.chars().take(w).collect();
    }
    let keep = w - 1;
    let head = keep / 2;
    let tail = keep - head;
    let mark = if st.unicode { '…' } else { '~' };
    let mut out: String = chars[..head].iter().collect();
    out.push(mark);
    out.extend(&chars[chars.len() - tail..]);
    out
}

/// A box `w` columns wide with `title` in its top edge and one row per line, each cut to fit.
pub fn boxed(st: &Style, title: &str, lines: &[String], color: Color, w: usize) -> Vec<String> {
    let g = &st.glyphs;
    let w = w.max(8);
    let inner = w - 4;
    let title = fit(title, inner.saturating_sub(2));
    let title = title.trim_end();
    let rest = w - 5 - visible_width(title);
    let mut out = vec![format!(
        "{}{}{}{}",
        st.paint(color, &format!("{}{}", g.top_left, g.horizontal)),
        st.strong(color, &format!(" {title} ")),
        st.paint(color, &g.horizontal.to_string().repeat(rest)),
        st.paint(color, &g.top_right.to_string())
    )];
    let side = st.paint(color, &g.vertical.to_string());
    for l in lines {
        out.push(format!("{side} {} {side}", fit(l, inner)));
    }
    out.push(st.paint(
        color,
        &format!(
            "{}{}{}",
            g.bottom_left,
            g.horizontal.to_string().repeat(w - 2),
            g.bottom_right
        ),
    ));
    out
}

/// Two boxes next to each other, one column apart; the shorter one is padded with blank rows inside its frame.
pub fn side_by_side(a: Vec<String>, b: Vec<String>) -> Vec<String> {
    let (wa, wb) = (
        a.first().map_or(0, |l| visible_width(l)),
        b.first().map_or(0, |l| visible_width(l)),
    );
    let n = a.len().max(b.len());
    (0..n)
        .map(|i| {
            let l = a.get(i).map_or(" ".repeat(wa), Clone::clone);
            let r = b.get(i).map_or(" ".repeat(wb), Clone::clone);
            format!("{l} {r}")
        })
        .collect()
}

/// A bar `w` cells wide filled to `f` (0..=1) in `color`, the rest gray.
pub fn bar(st: &Style, f: f64, w: usize, color: Color) -> String {
    let n = ((f.clamp(0.0, 1.0)) * w as f64).round() as usize;
    format!(
        "{}{}",
        st.paint(color, &st.glyphs.filled.to_string().repeat(n)),
        st.paint(Color::Gray, &st.glyphs.empty.to_string().repeat(w - n))
    )
}

/// A bar of `used` out of `limit` with the peak marked (`▏`) where it lies past the filled part.
pub fn gauge(st: &Style, used: f64, peak: f64, limit: f64, w: usize, color: Color) -> String {
    let cells = |v: f64| ((v / limit.max(1.0)).clamp(0.0, 1.0) * w as f64).round() as usize;
    let n = cells(used);
    let p = cells(peak).min(w.saturating_sub(1));
    let rest: String = (n..w)
        .map(|i| {
            if i == p && p > n {
                st.glyphs.peak
            } else {
                st.glyphs.empty
            }
        })
        .collect();
    format!(
        "{}{}",
        st.paint(color, &st.glyphs.filled.to_string().repeat(n)),
        st.paint(Color::Gray, &rest)
    )
}

/// Plain `s` broken at spaces into lines of at most `w` columns, the later ones indented by two; a word longer than a
/// line is cut.
pub fn wrap(s: &str, w: usize) -> Vec<String> {
    let w = w.max(8);
    let mut out = Vec::new();
    let mut line = String::new();
    for word in s.split(' ') {
        let indent = if out.is_empty() { 0 } else { 2 };
        let used = line.chars().count();
        if used > 0 && used + 1 + word.chars().count() > w - indent {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    out.push(line);
    (out.iter().enumerate())
        .map(|(i, l)| {
            let l = if i == 0 { l.clone() } else { format!("  {l}") };
            l.chars().take(w).collect()
        })
        .collect()
}

/// `items` joined with `, ` into lines of at most `w` columns, broken only between items (an item longer than a line
/// stands alone); the later lines indented by two.
pub fn wrap_list(items: &[String], w: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut line = String::new();
    for it in items {
        let indent = if out.is_empty() { 0 } else { 2 };
        let used = line.chars().count();
        if used > 0 && used + 2 + it.chars().count() > w.saturating_sub(indent) {
            out.push(std::mem::take(&mut line) + ",");
        }
        if !line.is_empty() {
            line.push_str(", ");
        } else if !out.is_empty() {
            line.push_str("  ");
        }
        line.push_str(it);
    }
    out.push(line);
    out
}

/// [`boxed`], but a line wider than the box is written whole past the right edge instead of cut: a path or a command
/// the user copies stays in one piece (the terminal wraps it).
pub fn boxed_whole(
    st: &Style,
    title: &str,
    lines: &[String],
    color: Color,
    w: usize,
) -> Vec<String> {
    let mut out = boxed(st, title, lines, color, w);
    let inner = w.max(8) - 4;
    let side = st.paint(color, &st.glyphs.vertical.to_string());
    for (i, l) in lines.iter().enumerate() {
        if visible_width(l) > inner {
            // +1: the top edge
            out[i + 1] = format!("{side} {l}");
        }
    }
    out
}
