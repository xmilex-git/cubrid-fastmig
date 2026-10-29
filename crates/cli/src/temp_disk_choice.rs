//! The temp disk limit of one migrate, decided once before anything is written (workspace#526).
//!
//! Input: `--temp-disk min|max|<size>` or nothing, the resource estimate (None for an 11.4 source), the memory
//! limit, the target space the plan needs, and the free space of the target and spill filesystems.
//! Output: the limit in bytes, or None for no limit (an 11.4 source without `--temp-disk`, as before).
//! Rules (user, 2026-10-30): no option on a terminal → show min and max and ask; no option otherwise → max when it
//! fits the free space, else refuse naming the choices; a limit below min or a limit plus the target growth over
//! the free space → refuse. Nothing is retried or grown during the run.
//! Source: workspace#526.

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::Path;

use fastmig_plan::free_space;

use crate::limits::TempDiskChoice;
use crate::resource_estimate::ResourceEstimate;

fn mib(b: u64) -> u64 {
    b.div_ceil(1 << 20)
}

fn refuse(msg: String) -> io::Error {
    io::Error::other(format!("temp disk: {msg}"))
}

/// Space a temp limit takes from: the target filesystem (with the target's own growth) when the spill directory
/// lies under the target directory, else the spill directory's filesystem alone.
fn room_for(
    limit: u64,
    needed: u64,
    target_available: u64,
    spill: &Path,
    target: &Path,
) -> io::Result<bool> {
    Ok(if spill.starts_with(target) {
        needed.saturating_add(limit) <= target_available
    } else {
        limit <= free_space::available_bytes(spill)?
    })
}

fn choices(e: &ResourceEstimate, mem: u64) -> String {
    let (lo, hi) = (e.temp_min(mem), e.temp_max(mem));
    let b = e.batches(lo, mem).unwrap_or(0);
    format!(
        "--temp-disk min ({} MiB, {b} index batches, about {:.1} s more) or --temp-disk max ({} MiB, one batch), or a size in between",
        mib(lo),
        e.extra_seconds(b),
        mib(hi)
    )
}

pub fn choose(
    choice: Option<TempDiskChoice>,
    est: Option<&ResourceEstimate>,
    mem: u64,
    needed: u64,
    target_available: u64,
    spill: &Path,
    target: &Path,
) -> io::Result<Option<u64>> {
    let Some(e) = est else {
        return match choice {
            None => Ok(None),
            Some(TempDiskChoice::Bytes(b)) => Ok(Some(b)),
            Some(_) => Err(refuse(
                "min and max need the resource estimate, which 11.4 sources do not have yet; give a size".into(),
            )),
        };
    };
    let (lo, hi) = (e.temp_min(mem), e.temp_max(mem));
    let limit = match choice {
        Some(TempDiskChoice::Min) => lo,
        Some(TempDiskChoice::Max) => hi,
        Some(TempDiskChoice::Bytes(b)) => b,
        None if io::stdin().is_terminal() && io::stderr().is_terminal() => ask(e, mem)?,
        None => {
            if !room_for(hi, needed, target_available, spill, target)? {
                return Err(refuse(format!(
                    "max ({} MiB) and the target's {} MiB do not fit the free space; choose {}",
                    mib(hi),
                    mib(needed),
                    choices(e, mem)
                )));
            }
            hi
        }
    };
    if limit < lo {
        return Err(refuse(format!(
            "{} MiB is below the minimum {} MiB (the largest index's spill bound)",
            mib(limit),
            mib(lo)
        )));
    }
    if !room_for(limit, needed, target_available, spill, target)? {
        return Err(refuse(format!(
            "{} MiB and the target's {} MiB do not fit the free space ({} MiB on the target filesystem)",
            mib(limit),
            mib(needed),
            mib(target_available)
        )));
    }
    Ok(Some(limit))
}

/// Shows the table and reads `min`, `max` or a size from the terminal (empty: max).
fn ask(e: &ResourceEstimate, mem: u64) -> io::Result<u64> {
    let mut err = io::stderr();
    write!(
        err,
        "{}Choose the temp disk limit of this run: {}.\ntemp disk [max]: ",
        e.table(mem),
        choices(e, mem)
    )?;
    err.flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    match line.trim() {
        "" | "max" => Ok(e.temp_max(mem)),
        "min" => Ok(e.temp_min(mem)),
        v => crate::limits::parse_size(v).ok_or_else(|| refuse(format!("bad answer {v:?}"))),
    }
}
