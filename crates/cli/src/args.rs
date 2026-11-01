//! The subcommands' option parser, in the shape of the CUBRID utilities (getopt_long): `--flag value`,
//! `--flag=value`, `--switch`, and the short forms of [`SHORT`] (`-F dir`, `-Fdir`, `-r`); and the error type the
//! subcommands return.
//!
//! Input: the arguments after the subcommand and the long flags and switches it accepts. Output: positional arguments
//! and the given options under their long names; an unknown option, a missing value or a repeated option is a usage
//! error, and a name the tool had before workspace#605 says which name replaced it.
//! Source: decision G13 workspace#604 (copydb/createdb names, `=` and short options in our own parser, no alias for
//! the old names), engine `src/executables/util_sa.c`/`utility.h` (copydb, createdb option tables).

use std::collections::HashMap;
use std::io;

pub enum CliError {
    Usage(String),
    Failed(io::Error),
}

impl From<io::Error> for CliError {
    fn from(e: io::Error) -> Self {
        CliError::Failed(e)
    }
}

/// Short option → its long name; a short is accepted where its long name is.
const SHORT: &[(char, &str)] = &[
    ('F', "--file-path"),
    ('L', "--log-path"),
    ('B', "--lob-base-path"),
    ('E', "--extended-volume-path"),
    ('i', "--control-file"),
    ('t', "--thread-count"),
    ('r', "--replace"),
    ('d', "--delete-source"),
    ('h', "--help"),
];

/// Names before workspace#605 → the name to use now (G13: no alias, the error names the replacement).
const RENAMED: &[(&str, &str)] = &[
    ("--dest", "-F, --file-path"),
    ("--log-dir", "-L, --log-path"),
    ("--lob-dir", "-B, --lob-base-path"),
    ("--workers", "-t, --thread-count"),
    ("--mem", "--temp-mem-size"),
    ("--temp-disk", "--temp-disk-size"),
    ("--verify", "--check"),
    ("--target-name", "the second positional argument <new-db>"),
];

pub struct Parsed {
    pub positional: Vec<String>,
    values: HashMap<String, String>,
}

impl Parsed {
    pub fn value(&self, flag: &str) -> Option<&str> {
        self.values.get(flag).map(String::as_str)
    }

    pub fn switch(&self, flag: &str) -> bool {
        self.values.contains_key(flag)
    }
}

fn unknown(name: &str) -> CliError {
    match RENAMED.iter().find(|(old, _)| *old == name) {
        Some((_, new)) => CliError::Usage(format!("unknown option {name} (renamed: use {new})")),
        None => CliError::Usage(format!("unknown option {name}")),
    }
}

/// `flags` take a value; `switches` take none. Both are long names; `--help` is always accepted.
pub fn parse(args: &[String], flags: &[&str], switches: &[&str]) -> Result<Parsed, CliError> {
    let mut positional = Vec::new();
    let mut values = HashMap::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        // (long name, value written in the same argument)
        let (name, inline): (String, Option<String>) = if let Some(long) = a.strip_prefix("--") {
            match long.split_once('=') {
                Some((n, v)) => (format!("--{n}"), Some(v.to_string())),
                None => (a.clone(), None),
            }
        } else if let Some(short) = a.strip_prefix('-').filter(|s| !s.is_empty()) {
            let mut chars = short.chars();
            let c = chars.next().unwrap_or_default();
            let long = SHORT
                .iter()
                .find(|(s, _)| *s == c)
                .map(|(_, l)| *l)
                .ok_or_else(|| CliError::Usage(format!("unknown option -{c}")))?;
            let rest: String = chars.collect();
            (long.to_string(), (!rest.is_empty()).then_some(rest))
        } else {
            positional.push(a.clone());
            continue;
        };
        let v = if flags.contains(&name.as_str()) {
            match inline {
                Some(v) => v,
                None => it
                    .next()
                    .ok_or_else(|| CliError::Usage(format!("{a} needs a value")))?
                    .clone(),
            }
        } else if switches.contains(&name.as_str()) || name == "--help" {
            if inline.is_some() {
                return Err(CliError::Usage(format!("{name} takes no value")));
            }
            String::new()
        } else {
            return Err(unknown(&name));
        };
        if values.insert(name.clone(), v).is_some() {
            return Err(CliError::Usage(format!("{name} given twice")));
        }
    }
    Ok(Parsed { positional, values })
}
