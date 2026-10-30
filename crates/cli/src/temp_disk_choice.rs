//! The temp disk limit of one migrate, decided once before anything is written (workspace#526).
//!
//! Input: `--temp-disk min|max|<size>` or nothing, the resource estimate, the memory limit (for the table), the
//! target space the plan needs, and the free space of the target and spill filesystems.
//! Output: the limit in bytes.
//! Rules (user, 2026-10-30): no option on a terminal → show min and max and ask; no option otherwise → max when it
//! fits the free space, else refuse naming the choices; with `--verify` min and max are the verifier's (it batches
//! its indexes like the build, on the target then on the source: `ResourceEstimate::verify_min`, `verify_max`,
//! workspace#553); a limit below min or a limit plus the target growth over the free space → refuse. Nothing is
//! retried or grown during the run.
//! Source: workspace#526, #553.

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

fn choices(e: &ResourceEstimate, verify: bool) -> String {
    if let (Some(v), Some(vmax), true) = (e.verify_min(), e.verify_max(), verify) {
        let (tb, sb) = e.verify_batches(v).unwrap_or((0, 0));
        return format!(
            "--temp-disk min ({} MiB; --verify: {tb} index batches on the target, {sb} on the source, about {:.1} s more) or --temp-disk max ({} MiB, one batch each), or a size in between",
            mib(v),
            e.verify_extra_seconds(v),
            mib(vmax)
        );
    }
    let (lo, hi) = (e.temp_min(), e.temp_max());
    let b = e.batches(lo).unwrap_or(0);
    format!(
        "--temp-disk min ({} MiB, {b} index batches, about {:.1} s more) or --temp-disk max ({} MiB, one batch), or a size in between",
        mib(lo),
        e.extra_seconds(b),
        mib(hi)
    )
}

#[allow(clippy::too_many_arguments)]
pub fn choose(
    choice: Option<TempDiskChoice>,
    e: &ResourceEstimate,
    mem: u64,
    verify: bool,
    needed: u64,
    target_available: u64,
    spill: &Path,
    target: &Path,
) -> io::Result<u64> {
    // the verifier runs after the build's spill is gone and batches its indexes the same way (docs/resources.md,
    // workspace#553): its least and most cover the build's
    let (lo, hi) = match (e.verify_min(), e.verify_max()) {
        (Some(v), Some(vmax)) if verify => (v, vmax),
        _ => (e.temp_min(), e.temp_max()),
    };
    let limit = match choice {
        Some(TempDiskChoice::Min) => lo,
        Some(TempDiskChoice::Max) => hi,
        Some(TempDiskChoice::Bytes(b)) => b,
        None if io::stdin().is_terminal() && io::stderr().is_terminal() => {
            ask(e, mem, verify, lo, hi)?
        }
        None => {
            if !room_for(hi, needed, target_available, spill, target)? {
                return Err(refuse(format!(
                    "max ({} MiB) and the target's {} MiB do not fit the free space; choose {}",
                    mib(hi),
                    mib(needed),
                    choices(e, verify)
                )));
            }
            hi
        }
    };
    if limit < lo {
        return Err(refuse(format!(
            "{} MiB is below the minimum {} MiB ({})",
            mib(limit),
            mib(lo),
            if verify {
                "--verify: the largest index of the target or the source"
            } else {
                "the largest index's pair bytes"
            }
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
    Ok(limit)
}

/// Shows the table and reads `min`, `max` or a size from the terminal (empty: max).
fn ask(e: &ResourceEstimate, mem: u64, verify: bool, lo: u64, hi: u64) -> io::Result<u64> {
    let mut err = io::stderr();
    write!(
        err,
        "{}Choose the temp disk limit of this run: {}.\ntemp disk [max]: ",
        e.table(mem),
        choices(e, verify)
    )?;
    err.flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    match line.trim() {
        "" | "max" => Ok(hi),
        "min" => Ok(lo),
        v => crate::limits::parse_size(v).ok_or_else(|| refuse(format!("bad answer {v:?}"))),
    }
}
