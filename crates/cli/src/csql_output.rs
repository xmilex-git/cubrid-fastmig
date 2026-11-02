//! Reads the files of one csql run (`engine_sa`) line by line: the first failed statement from stderr, the rows of
//! `-q -N` output as fields from the `-o` file, and the query plans from stdout.
//!
//! Input: the run's `.err`, `.out` or `.log` file. Output: [`first_error`], [`rows`], [`tagged_rows`],
//! [`index_scans`]. No file is read whole into memory.
//! Facts (develop and 11.4 csql, probed in workspace#611 scratch/611 p1, p5, probe-o): `csql -i` runs on after a
//! failed statement and exits 0, with or without `-e`, so a failure is an `ERROR` line (e2e/run.sh `sql_file`,
//! workspace#555); with `-o <file>` the results go to that file, every `ERROR` line and the line naming the statement
//! (`In line <n>, column <m>,` for a parse error, `In the command from line <n>,` for an execution error) go to
//! stderr, and the plans of `;plan simple` (`Query plan:` and the plan) go to stdout; `-q -N` prints a row as
//! comma-separated SQL literals (`'it''s'`, `NULL`, `1.500000000000000e+00`, `X'0f'`).

use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::Path;

/// The first failed statement: its line in the input file, when csql named it, and the error text.
pub struct SqlError {
    pub line: Option<usize>,
    pub text: String,
}

fn lines(file: &Path) -> io::Result<impl Iterator<Item = String>> {
    Ok(BufReader::new(File::open(file)?)
        .lines()
        .map_while(Result::ok))
}

/// `In line <n>, column <m>,` (a parse error) or `In the command from line <n>,` (an execution error).
fn line_of(s: &str) -> Option<usize> {
    let at = ["In line ", "from line "]
        .iter()
        .find_map(|p| Some(s.find(p)? + p.len()))?;
    s[at..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()
}

/// The first `ERROR` of a csql stderr file, with the statement line csql printed before it and the message under it.
pub fn first_error(err_file: &Path) -> io::Result<Option<SqlError>> {
    let mut line = None;
    let mut it = lines(err_file)?;
    while let Some(l) = it.next() {
        if l.trim_start().starts_with("ERROR") {
            let line = line_of(&l).or(line);
            let next = it.find(|n| !n.trim().is_empty()).unwrap_or_default();
            let text = format!("{} {}", l.trim(), next.trim()).trim().to_string();
            return Ok(Some(SqlError { line, text }));
        }
        if let Some(n) = line_of(&l) {
            line = Some(n);
        }
    }
    Ok(None)
}

/// The last non-empty line of a file (a utility's last message), empty when there is none.
pub fn last_line(file: &Path) -> io::Result<String> {
    Ok(lines(file)?
        .filter(|l| !l.trim().is_empty())
        .last()
        .unwrap_or_default())
}

/// The fields of one `-q -N` row: `Some(literal)` as printed (a string keeps its quotes), `None` for NULL.
pub fn fields(row: &str) -> Vec<Option<String>> {
    let mut out = Vec::new();
    let b = row.as_bytes();
    let mut i = 0;
    loop {
        let start = i;
        if b.get(i) == Some(&b'\'') || (b.get(i) == Some(&b'X') && b.get(i + 1) == Some(&b'\'')) {
            i += if b[i] == b'X' { 2 } else { 1 };
            // a quote ends the literal unless doubled
            while i < b.len() {
                if b[i] == b'\'' {
                    if b.get(i + 1) == Some(&b'\'') {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
        }
        while i < b.len() && b[i] != b',' {
            i += 1;
        }
        let f = &row[start..i];
        out.push((f != "NULL").then(|| f.to_string()));
        if i >= b.len() {
            return out;
        }
        i += 1;
    }
}

/// The text of a quoted literal (`'it''s'` → `it's`); other literals as printed.
pub fn unquote(literal: &str) -> String {
    match literal
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
    {
        Some(s) => s.replace("''", "'"),
        None => literal.to_string(),
    }
}

/// Every row of a `-q -N` result file whose first field is a string (catalog queries), unquoted.
pub fn rows(out_file: &Path) -> io::Result<Vec<Vec<Option<String>>>> {
    Ok(lines(out_file)?
        .filter(|l| l.starts_with('\''))
        .map(|l| {
            fields(&l)
                .into_iter()
                .map(|f| f.map(|v| unquote(&v)))
                .collect()
        })
        .collect())
}

/// Rows of a `-q -N` result file that start with a quoted tag of `prefix` (`'i12'`, prefix `i`): tag → the fields
/// after it.
pub fn tagged_rows(
    out_file: &Path,
    prefix: &str,
) -> io::Result<Vec<(String, Vec<Option<String>>)>> {
    let head = format!("'{prefix}");
    let mut out = Vec::new();
    for l in lines(out_file)? {
        if !l.starts_with(&head) {
            continue;
        }
        let mut f = fields(&l);
        if let Some(Some(tag)) = f.first().cloned() {
            f.remove(0);
            out.push((unquote(&tag), f));
        }
    }
    Ok(out)
}

/// The plan lines of a csql stdout file that are an index scan: `Index scan(<table> <table>, <index>, ...`.
pub fn index_scans(log_file: &Path) -> io::Result<Vec<String>> {
    Ok(lines(log_file)?
        .map(|l| l.trim().to_string())
        .filter(|l| l.starts_with("Index scan("))
        .collect())
}
