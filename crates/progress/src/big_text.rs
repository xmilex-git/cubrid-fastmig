//! Three-row letters drawn with box characters: the `cubrid-fastmig` logo on top of a screen and the title of the
//! required SQL box.
//!
//! Input: lowercase text of the letters below, `-` and space. Output: three rows of equal width; a character without
//! a glyph is left out. Unicode only: an ASCII screen shows no logo and a plain title.
//! Source: workspace#610 decisions 5 and 9; the glyph shapes follow figlet's `future` font.

/// Rows of one glyph.
fn glyph(c: char) -> Option<[&'static str; 3]> {
    Some(match c {
        'a' => ["┏━┓", "┣━┫", "╹ ╹"],
        'b' => ["┏┓ ", "┣┻┓", "┗━┛"],
        'c' => ["┏━╸", "┃  ", "┗━╸"],
        'd' => ["╺┳┓", " ┃┃", "╺┻┛"],
        'e' => ["┏━╸", "┣╸ ", "┗━╸"],
        'f' => ["┏━╸", "┣╸ ", "╹  "],
        'g' => ["┏━╸", "┃╺┓", "┗━┛"],
        'i' => ["╻", "┃", "╹"],
        'l' => ["╻  ", "┃  ", "┗━╸"],
        'm' => ["┏┳┓", "┃┃┃", "╹ ╹"],
        'n' => ["┏┓╻", "┃┗┫", "╹ ╹"],
        'o' => ["┏━┓", "┃ ┃", "┗━┛"],
        'q' => ["┏━┓", "┃┓┃", "┗┻┛"],
        'r' => ["┏━┓", "┣┳┛", "╹┗╸"],
        's' => ["┏━┓", "┗━┓", "┗━┛"],
        't' => ["╺┳╸", " ┃ ", " ╹ "],
        'u' => ["╻ ╻", "┃ ┃", "┗━┛"],
        '-' => ["   ", "╺━╸", "   "],
        ' ' => ["  ", "  ", "  "],
        _ => return None,
    })
}

/// The three rows of `text`.
pub fn rows(text: &str) -> [String; 3] {
    let mut out = [String::new(), String::new(), String::new()];
    for g in text.chars().filter_map(|c| glyph(c.to_ascii_lowercase())) {
        for (row, part) in out.iter_mut().zip(g) {
            row.push_str(part);
        }
    }
    out
}
