//! How the terminal screens paint: colors on or off, Unicode or ASCII characters, and the terminal's size.
//!
//! Input: `--color auto|always|never`, `--ascii`, whether the stream is a terminal, `NO_COLOR`, the locale variables.
//! Output: [`Style`], whose [`Style::paint`] wraps text in one of the 16 basic colors (or leaves it plain) and whose
//! characters are the rounded box and `█░` bar, or `+-|` and `#.` with `--ascii` or a locale that names a codeset
//! other than UTF-8 (`ko_KR.eucKR`); an empty, `C` or `POSIX` locale draws Unicode (a LANG=C server is often reached
//! from a UTF-8 terminal).
//! Colors: done green, running cyan, waiting gray, warning yellow, error red (workspace#610 decision 3).
//! Source: workspace#610 decisions 3 and 4 (4 as changed by the user 2026-11-02); <https://no-color.org> (`NO_COLOR` set and not empty turns color off);
//! POSIX locale precedence `LC_ALL`, `LC_CTYPE`, `LANG`; Linux `TIOCGWINSZ` (asm-generic/ioctls.h 0x5413).

use std::io::IsTerminal;

/// One of the 16 basic colors (SGR 30-37, 90-97), or bold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Color {
    Green,
    Cyan,
    Gray,
    Yellow,
    Red,
    Blue,
    Magenta,
    Bold,
}

impl Color {
    fn sgr(self) -> &'static str {
        match self {
            Color::Green => "92",
            Color::Cyan => "96",
            Color::Gray => "90",
            Color::Yellow => "93",
            Color::Red => "91",
            Color::Blue => "94",
            Color::Magenta => "95",
            Color::Bold => "1",
        }
    }
}

/// The characters one box and one bar are drawn with.
#[derive(Clone, Copy, Debug)]
pub struct Glyphs {
    pub top_left: char,
    pub top_right: char,
    pub bottom_left: char,
    pub bottom_right: char,
    pub horizontal: char,
    pub vertical: char,
    pub filled: char,
    pub empty: char,
    /// The peak mark inside a gauge.
    pub peak: char,
    pub arrow: &'static str,
    /// The thin arrow inside a row (`6 views → <file>`).
    pub to: &'static str,
    pub done: &'static str,
    pub running: &'static str,
    pub waiting: &'static str,
    pub warning: &'static str,
    pub dot: &'static str,
}

const UNICODE: Glyphs = Glyphs {
    top_left: '╭',
    top_right: '╮',
    bottom_left: '╰',
    bottom_right: '╯',
    horizontal: '─',
    vertical: '│',
    filled: '█',
    empty: '░',
    peak: '▏',
    arrow: "━━▶",
    to: "→",
    done: "✔",
    running: "▶",
    waiting: "○",
    warning: "▲",
    dot: "·",
};

const ASCII: Glyphs = Glyphs {
    top_left: '+',
    top_right: '+',
    bottom_left: '+',
    bottom_right: '+',
    horizontal: '-',
    vertical: '|',
    filled: '#',
    empty: '.',
    peak: '|',
    arrow: "->",
    to: "->",
    done: "+",
    running: ">",
    waiting: "o",
    warning: "!",
    dot: "-",
};

#[derive(Clone, Copy, Debug)]
pub struct Style {
    pub color: bool,
    pub unicode: bool,
    pub glyphs: Glyphs,
}

/// Which standard stream a screen is written to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

impl Stream {
    fn fd(self) -> i32 {
        match self {
            Stream::Stdout => 1,
            Stream::Stderr => 2,
        }
    }

    pub fn is_terminal(self) -> bool {
        match self {
            Stream::Stdout => std::io::stdout().is_terminal(),
            Stream::Stderr => std::io::stderr().is_terminal(),
        }
    }
}

impl Style {
    /// `color_arg`: the `--color` value (`None` = auto); `ascii`: `--ascii`. `None` back for a bad `--color` value.
    pub fn detect(color_arg: Option<&str>, ascii: bool, stream: Stream) -> Option<Style> {
        let color = match color_arg.unwrap_or("auto") {
            "always" => true,
            "never" => false,
            "auto" => {
                stream.is_terminal() && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty())
            }
            _ => return None,
        };
        let unicode = !ascii && !non_utf8_locale();
        Some(Style {
            color,
            unicode,
            glyphs: if unicode { UNICODE } else { ASCII },
        })
    }

    /// No color, ASCII: for text that goes to a file.
    pub fn plain() -> Style {
        Style {
            color: false,
            unicode: false,
            glyphs: ASCII,
        }
    }

    pub fn paint(&self, c: Color, s: &str) -> String {
        if self.color && !s.is_empty() {
            format!("\x1b[{}m{s}\x1b[0m", c.sgr())
        } else {
            s.to_string()
        }
    }

    /// Bold and a color.
    pub fn strong(&self, c: Color, s: &str) -> String {
        if self.color && !s.is_empty() {
            format!("\x1b[1;{}m{s}\x1b[0m", c.sgr())
        } else {
            s.to_string()
        }
    }
}

/// The first of `LC_ALL`, `LC_CTYPE`, `LANG` that is set and not empty names a codeset (`lang_TERRITORY.codeset`)
/// that is not UTF-8.
fn non_utf8_locale() -> bool {
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .find_map(|v| std::env::var(v).ok().filter(|s| !s.is_empty()))
        .and_then(|l| {
            let codeset = l.split_once('.')?.1;
            let codeset = codeset.split('@').next().unwrap_or("").to_ascii_lowercase();
            Some(codeset != "utf-8" && codeset != "utf8")
        })
        .unwrap_or(false)
}

/// (columns, rows) of the terminal on `stream`; (100, 40) when it cannot be read.
pub fn term_size(stream: Stream) -> (usize, usize) {
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
    let rc = unsafe { ioctl(stream.fd(), TIOCGWINSZ, &mut ws as *mut Winsize) };
    if rc == 0 && ws.col > 0 && ws.row > 0 {
        (ws.col as usize, ws.row as usize)
    } else {
        (100, 40)
    }
}
