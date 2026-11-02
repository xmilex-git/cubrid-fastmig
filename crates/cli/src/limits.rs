//! The process-wide resource limits every subcommand shares: `--temp-mem-size` (the memory limit; default the target
//! conf's `data_buffer_size` for plan and migrate, else a quarter of the physical memory, at least 256 MiB where half
//! the memory allows) and `--spill-dir` (where stages that exceed it spill; default `<-F>/fastmig/spill`).
//!
//! Input: the parsed `--temp-mem-size` / `--spill-dir` values, the target directory and the conf default. Output:
//! [`Limits`]. `--temp-mem-size` takes bytes with an optional K, M, G or T suffix (powers of 1024).
//! Files under the spill directory are never fsynced (see `fastmig_copy::durable`).
//! Source: decision G1 workspace#481 (one memory limit for the whole run, like `--thread-count`); G13 workspace#604 (names, the conf default:
//! the engine's data buffer holds the temp file pages too); the copy engine streams through
//! fixed per-worker buffers, the index builder and verifier spill (workspace#493, #495).

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fastmig_copy::volume_copy::IO_BUF_BYTES;
use fastmig_index::intermediate_merge::WORKER_MIN;
use fastmig_index::spill_arena::SpillArena;

use crate::args::{CliError, Parsed};
use crate::question_screen::Question;
use crate::resource_estimate::ResourceEstimate;

pub const FLAGS: &[&str] = &["--temp-mem-size", "--spill-dir"];

#[derive(Clone)]
pub struct Limits {
    pub memory_limit_bytes: u64,
    pub spill_dir: PathBuf,
    /// The temp disk limit migrate chose before the start (workspace#526); None: one index batch (no limit, or one that holds every index at once).
    pub temp_disk: Option<u64>,
    /// The spill arena of that limit, allocated before the first write; every spill goes into it.
    pub arena: Option<Arc<SpillArena>>,
}

impl Limits {
    /// `conf_default`: the target conf's `data_buffer_size` (plan, migrate), none for check.
    pub fn from(
        p: &Parsed,
        target_dir: &Path,
        conf_default: Option<u64>,
    ) -> Result<Limits, CliError> {
        let memory_limit_bytes = match (p.value("--temp-mem-size"), conf_default) {
            (Some(v), _) => {
                parse_size(v).ok_or_else(|| CliError::Usage(format!("bad --temp-mem-size {v}")))?
            }
            (None, Some(c)) => c,
            // at least 256 MiB (at most half the memory): from there every measured run stayed within --temp-mem-size
            // (docs/resources.md, "Memory outside --temp-mem-size")
            (None, None) => {
                let p = physical_memory()?;
                (p / 4).max((256 << 20).min(p / 2))
            }
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

/// The smallest `--temp-mem-size` a run of `workers` workers takes: per worker the copy engine's I/O buffer or what the index
/// build needs (`intermediate_merge::WORKER_MIN`), whichever is more (docs/resources.md).
pub fn mem_min(workers: usize) -> u64 {
    (workers * IO_BUF_BYTES.max(WORKER_MIN)) as u64
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

/// `-t, --thread-count` (worker limit): the thread cap of the whole run; default half the CPUs.
pub fn worker_limit(p: &Parsed) -> Result<usize, CliError> {
    match p.value("--thread-count") {
        Some(w) => w
            .parse()
            .ok()
            .filter(|&n| n > 0)
            .ok_or_else(|| CliError::Usage(format!("bad --thread-count {w}"))),
        None => Ok(std::thread::available_parallelism().map_or(1, |n| (n.get() / 2).max(1))),
    }
}

/// `--temp-disk-size min|max|<size>` (temp disk limit, workspace#526): chosen before the run starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TempDiskChoice {
    Min,
    Max,
    Bytes(u64),
}

pub fn temp_disk_choice(p: &Parsed) -> Result<Option<TempDiskChoice>, CliError> {
    Ok(match p.value("--temp-disk-size") {
        None => None,
        Some("min") => Some(TempDiskChoice::Min),
        Some("max") => Some(TempDiskChoice::Max),
        Some(v) => Some(TempDiskChoice::Bytes(parse_size(v).ok_or_else(|| {
            CliError::Usage(format!("bad --temp-disk-size {v} (min, max or a size)"))
        })?)),
    })
}

/// The memory limit's question on the question screen (workspace#612): min the run's fixed buffers (with `--check`
/// the verifier's floor, `verify_mem_min`), recommended `mem_recommended`, max and free the physical memory.
pub fn memory_question(
    e: &ResourceEstimate,
    verify: bool,
    preset: Option<(u64, &'static str)>,
) -> Question {
    let min = match e.verify_mem_min() {
        Some(v) if verify => v,
        _ => e.mem_min(),
    };
    Question {
        name: "temp mem",
        min,
        recommended: e.mem_recommended().max(min),
        max: e.physical_memory,
        free: e.physical_memory,
        free_means: "free is the physical memory".into(),
        extra: None,
        preset,
    }
}
