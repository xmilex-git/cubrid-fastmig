//! A minimal `--flag value` / `--switch` parser for the subcommands, and the error type they return.
//!
//! Input: the arguments after the subcommand and the flags and switches it accepts. Output: positional arguments
//! and the given flags; an unknown flag, a missing value or a repeated flag is a usage error.

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

/// `flags` take a value; `switches` take none.
pub fn parse(args: &[String], flags: &[&str], switches: &[&str]) -> Result<Parsed, CliError> {
    let mut positional = Vec::new();
    let mut values = HashMap::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if !a.starts_with("--") {
            positional.push(a.clone());
            continue;
        }
        let v = if flags.contains(&a.as_str()) {
            it.next()
                .ok_or_else(|| CliError::Usage(format!("{a} needs a value")))?
                .clone()
        } else if switches.contains(&a.as_str()) {
            String::new()
        } else {
            return Err(CliError::Usage(format!("unknown option {a}")));
        };
        if values.insert(a.clone(), v).is_some() {
            return Err(CliError::Usage(format!("{a} given twice")));
        }
    }
    Ok(Parsed { positional, values })
}
