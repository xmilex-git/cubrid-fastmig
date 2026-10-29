//! Splits stored SQL text (a view query spec, a trigger condition or action, PL/CSQL source) into identifier and
//! punctuation tokens with their byte spans, so a later pass can find class-name positions and replace a name
//! without touching the rest of the text.
//!
//! Input: the text's bytes. Output: [`Token`]s in text order; string literals, numbers and comments yield none.
//! Rules: CUBRID identifier quoting `[..]`, `".."` (`ansi_quotes=yes`, the default) and `` `..` ``; names compare
//! ASCII case-insensitively; comments `--`, `//` to the end of the line and `/* .. */`; a string literal is `'..'`
//! with `''` for a quote (`no_backslash_escapes=yes`, the default). No SQL is parsed here.
//! Source: engine `csql_lexer.l` (develop) identifier, comment and string rules; workspace#542 item 1 (one module,
//! shared by the system name mapping and the later owner qualification of workspace#517).

/// One token of SQL text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Token {
    /// An identifier or keyword: `start..end` is the whole token with its quotes, `name_start..name_end` the name.
    Ident {
        start: usize,
        end: usize,
        name_start: usize,
        name_end: usize,
        quoted: bool,
    },
    /// A one-byte punctuation mark (`.`, `,`, `(`, `)`, `;`, operators byte by byte).
    Punct(u8, usize),
}

impl Token {
    /// The identifier's name bytes in `text`, or `None` for punctuation.
    pub fn name<'t>(&self, text: &'t [u8]) -> Option<&'t [u8]> {
        match *self {
            Token::Ident {
                name_start,
                name_end,
                ..
            } => Some(&text[name_start..name_end]),
            Token::Punct(..) => None,
        }
    }

    /// An unquoted identifier equal to `word` (ASCII case-insensitive): a keyword position.
    pub fn is_word(&self, text: &[u8], word: &str) -> bool {
        matches!(self, Token::Ident { quoted: false, .. })
            && self
                .name(text)
                .is_some_and(|n| n.eq_ignore_ascii_case(word.as_bytes()))
    }

    pub fn is_punct(&self, p: u8) -> bool {
        matches!(*self, Token::Punct(b, _) if b == p)
    }
}

fn starts_ident(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b >= 0x80
}

fn continues_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'#' | b'$') || b >= 0x80
}

/// Index just past the first `close` at or after `from`, or the text's end when the quote is never closed.
fn past(text: &[u8], from: usize, close: &[u8]) -> usize {
    text[from.min(text.len())..]
        .windows(close.len())
        .position(|w| w == close)
        .map_or(text.len(), |p| from + p + close.len())
}

/// The tokens of `text` (see the module header).
pub fn tokens(text: &[u8]) -> Vec<Token> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(&b) = text.get(i) {
        let next = text.get(i + 1).copied();
        match b {
            _ if b.is_ascii_whitespace() => i += 1,
            b'-' if next == Some(b'-') => i = past(text, i, b"\n"),
            b'/' if next == Some(b'/') => i = past(text, i, b"\n"),
            b'/' if next == Some(b'*') => i = past(text, i + 2, b"*/"),
            b'\'' => {
                // `''` inside a literal is a quote: keep going past each pair
                let mut j = i + 1;
                loop {
                    j = past(text, j, b"'");
                    if text.get(j) != Some(&b'\'') {
                        break;
                    }
                    j += 1;
                }
                i = j;
            }
            b'[' | b'"' | b'`' => {
                let close = match b {
                    b'[' => b']',
                    q => q,
                };
                let end = past(text, i + 1, &[close]);
                let name_end = if text.get(end.wrapping_sub(1)) == Some(&close) && end > i + 1 {
                    end - 1
                } else {
                    end
                };
                out.push(Token::Ident {
                    start: i,
                    end,
                    name_start: i + 1,
                    name_end,
                    quoted: true,
                });
                i = end;
            }
            _ if starts_ident(b) => {
                let end = text[i..]
                    .iter()
                    .position(|&c| !continues_ident(c))
                    .map_or(text.len(), |p| i + p);
                out.push(Token::Ident {
                    start: i,
                    end,
                    name_start: i,
                    name_end: end,
                    quoted: false,
                });
                i = end;
            }
            _ if b.is_ascii_digit() => {
                i = text[i..]
                    .iter()
                    .position(|&c| !(c.is_ascii_alphanumeric() || c == b'.'))
                    .map_or(text.len(), |p| i + p);
            }
            _ => {
                out.push(Token::Punct(b, i));
                i += 1;
            }
        }
    }
    out
}
