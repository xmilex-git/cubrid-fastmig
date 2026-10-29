//! The process-wide resource limits every subcommand shares: `--mem` (the memory limit; default a quarter of the
//! physical memory) and `--spill-dir` (where stages that exceed it spill; default `<dest>/fastmig/spill`).
//!
//! Input: the parsed `--mem` / `--spill-dir` values and the target directory. Output: [`Limits`].
//! `--mem` takes bytes with an optional K, M, G or T suffix (powers of 1024).
//! Files under the spill directory are never fsynced (see `fastmig_copy::durable`).
//! Source: decision G1 workspace#481 (one memory limit for the whole run, like `--workers`); the copy engine streams through
//! fixed per-worker buffers, the index builder and verifier spill (workspace#493, #495).

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fastmig_index::spill_arena::SpillArena;

use crate::args::{CliError, Parsed};

pub const FLAGS: &[&str] = &["--mem", "--spill-dir"];

#[derive(Clone)]
pub struct Limits {
    pub memory_limit_bytes: u64,
    pub spill_dir: PathBuf,
    /// The temp disk limit migrate chose before the start (workspace#526); None: no limit, one index batch.
    pub temp_disk: Option<u64>,
    /// The spill arena of that limit, allocated before the first write; every spill goes into it.
    pub arena: Option<Arc<SpillArena>>,
}

impl Limits {
    pub fn from(p: &Parsed, target_dir: &Path) -> Result<Limits, CliError> {
        let memory_limit_bytes = match p.value("--mem") {
            Some(v) => parse_size(v).ok_or_else(|| CliError::Usage(format!("bad --mem {v}")))?,
            None => physical_memory()? / 4,
        };
        let spill_dir = match p.value("--spill-dir") {
            Some(d) => std::path::absolute(d)?,
            None => target_dir.join("fastmig").join("spill"),
        };
        Ok(Limits {
            memory_limit_bytes,
            spill_dir,
            temp_disk: None,
            arena: None,
        })
    }
}

pub fn parse_size(v: &str) -> Option<u64> {
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
pub fn physical_memory() -> io::Result<u64> {
    let info = std::fs::read_to_string("/proc/meminfo")?;
    info.lines()
        .find_map(|l| l.strip_prefix("MemTotal:"))
        .and_then(|rest| rest.trim().strip_suffix("kB"))
        .and_then(|kb| kb.trim().parse::<u64>().ok())
        .map(|kb| kb * 1024)
        .ok_or_else(|| io::Error::other("no MemTotal in /proc/meminfo"))
}

/// `--workers` (worker limit): the thread cap of the whole run; default half the CPUs.
pub fn worker_limit(p: &Parsed) -> Result<usize, CliError> {
    match p.value("--workers") {
        Some(w) => w
            .parse()
            .ok()
            .filter(|&n| n > 0)
            .ok_or_else(|| CliError::Usage(format!("bad --workers {w}"))),
        None => Ok(std::thread::available_parallelism().map_or(1, |n| (n.get() / 2).max(1))),
    }
}

/// `--temp-disk min|max|<size>` (temp disk limit, workspace#526): chosen before the run starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TempDiskChoice {
    Min,
    Max,
    Bytes(u64),
}

pub fn temp_disk_choice(p: &Parsed) -> Result<Option<TempDiskChoice>, CliError> {
    Ok(match p.value("--temp-disk") {
        None => None,
        Some("min") => Some(TempDiskChoice::Min),
        Some("max") => Some(TempDiskChoice::Max),
        Some(v) => Some(TempDiskChoice::Bytes(parse_size(v).ok_or_else(|| {
            CliError::Usage(format!("bad --temp-disk {v} (min, max or a size)"))
        })?)),
    })
}
