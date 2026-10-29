//! The process-wide resource limits every subcommand shares: `--mem` (the memory cap; default a quarter of the
//! physical memory) and `--spill-dir` (where stages that exceed it spill; default `<dest>/fastmig/spill`).
//!
//! Input: the parsed `--mem` / `--spill-dir` values and the target directory. Output: [`Limits`].
//! `--mem` takes bytes with an optional K, M, G or T suffix (powers of 1024).
//! Files under the spill directory are never fsynced (see `fastmig_copy::durable`).
//! Source: decision G1 workspace#481 (one cap for the whole run, like `--workers`); the copy engine streams through
//! fixed per-worker buffers, the index builder and verifier spill (workspace#493, #495).

use std::io;
use std::path::{Path, PathBuf};

use crate::args::{CliError, Parsed};

pub const FLAGS: &[&str] = &["--mem", "--spill-dir"];

pub struct Limits {
    pub mem_bytes: u64,
    pub spill_dir: PathBuf,
}

impl Limits {
    pub fn from(p: &Parsed, dest: &Path) -> Result<Limits, CliError> {
        let mem_bytes = match p.value("--mem") {
            Some(v) => parse_size(v).ok_or_else(|| CliError::Usage(format!("bad --mem {v}")))?,
            None => physical_memory()? / 4,
        };
        let spill_dir = match p.value("--spill-dir") {
            Some(d) => std::path::absolute(d)?,
            None => dest.join("fastmig").join("spill"),
        };
        Ok(Limits {
            mem_bytes,
            spill_dir,
        })
    }
}

fn parse_size(v: &str) -> Option<u64> {
    let (num, shift) = match v.chars().last()?.to_ascii_uppercase() {
        'K' => (&v[..v.len() - 1], 10),
        'M' => (&v[..v.len() - 1], 20),
        'G' => (&v[..v.len() - 1], 30),
        'T' => (&v[..v.len() - 1], 40),
        _ => (v, 0),
    };
    num.parse::<u64>()
        .ok()?
        .checked_mul(1 << shift)
        .filter(|&b| b > 0)
}

/// `MemTotal` from /proc/meminfo, in bytes.
fn physical_memory() -> io::Result<u64> {
    let info = std::fs::read_to_string("/proc/meminfo")?;
    info.lines()
        .find_map(|l| l.strip_prefix("MemTotal:"))
        .and_then(|rest| rest.trim().strip_suffix("kB"))
        .and_then(|kb| kb.trim().parse::<u64>().ok())
        .map(|kb| kb * 1024)
        .ok_or_else(|| io::Error::other("no MemTotal in /proc/meminfo"))
}
