//! The temp disk limit of one migrate, decided once before anything is written (workspace#526).
//!
//! Input: `--temp-disk-size min|max|<size>` or nothing, the resource estimate, the target space the plan needs, and
//! the free space of the target and spill filesystems.
//! Output: the limit in bytes, or the [`Question`] the question screen asks it with.
//! Rules (user, 2026-10-30): no option on a terminal → the question screen asks (`question`: recommended max, or the
//! most that fits the free space, workspace#612); no option otherwise → max when it fits the free space, else refuse
//! naming the choices; with `--check` min and max are the verifier's (it batches
//! its indexes like the build, on the target then on the source: `ResourceEstimate::verify_min`, `verify_max`,
//! workspace#553); a limit below min or a limit plus the target growth over the free space → refuse. Nothing is
//! retried or grown during the run.
//! Source: workspace#526, #553.

use std::io;
use std::path::Path;

use fastmig_plan::free_space;
use fastmig_progress::unit;

use crate::limits::TempDiskChoice;
use crate::question_screen::Question;
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
            "--temp-disk-size min ({} MiB; --check: {tb} index batches on the target, {sb} on the source, about {:.1} s more) or --temp-disk-size max ({} MiB, one batch each), or a size in between",
            mib(v),
            e.verify_extra_seconds(v),
            mib(vmax)
        );
    }
    let (lo, hi) = (e.temp_min(), e.temp_max());
    let b = e.batches(lo).unwrap_or(0);
    format!(
        "--temp-disk-size min ({} MiB, {b} index batches, about {:.1} s more) or --temp-disk-size max ({} MiB, one batch), or a size in between",
        mib(lo),
        e.extra_seconds(b),
        mib(hi)
    )
}

/// (min, max) of the run's limit.
fn range(e: &ResourceEstimate, verify: bool) -> (u64, u64) {
    // the verifier runs after the build's spill is gone and batches its indexes the same way (docs/resources.md,
    // workspace#553): its least and most cover the build's
    match (e.verify_min(), e.verify_max()) {
        (Some(v), Some(vmax)) if verify => (v, vmax),
        _ => (e.temp_min(), e.temp_max()),
    }
}

/// `choice` checked against min and the free space; no choice: max when it fits (the rule without a terminal).
pub fn choose(
    choice: Option<TempDiskChoice>,
    e: &ResourceEstimate,
    verify: bool,
    needed: u64,
    target_available: u64,
    spill: &Path,
    target: &Path,
) -> io::Result<u64> {
    let (lo, hi) = range(e, verify);
    let limit = match choice {
        Some(TempDiskChoice::Min) => lo,
        Some(TempDiskChoice::Max) => hi,
        Some(TempDiskChoice::Bytes(b)) => b,
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
                "--check: the largest index of the target or the source"
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

/// The question of the screen: free is the room a limit has (the target filesystem less the target's growth when the
/// spill directory lies under the target, else the spill filesystem); recommended is max, or that room when max does
/// not fit; refused when not even min fits. `preset`: the limit an option or the conf set, shown instead of asked.
#[allow(clippy::too_many_arguments)]
pub fn question(
    e: &ResourceEstimate,
    verify: bool,
    needed: u64,
    target_available: u64,
    spill: &Path,
    target: &Path,
    preset: Option<(u64, &'static str)>,
) -> io::Result<Question> {
    let (lo, hi) = range(e, verify);
    let (free, free_means) = if spill.starts_with(target) {
        (
            target_available.saturating_sub(needed),
            format!(
                "free is the target filesystem's {} less the target's own {}",
                unit::bytes(target_available as f64),
                unit::bytes(needed as f64)
            ),
        )
    } else {
        (
            free_space::available_bytes(spill)?,
            format!("free is the spill filesystem's ({})", spill.display()),
        )
    };
    if preset.is_none() && free < lo {
        return Err(refuse(format!(
            "min ({} MiB) and the target's {} MiB do not fit the free space ({} MiB on the target filesystem)",
            mib(lo),
            mib(needed),
            mib(target_available)
        )));
    }
    // index batches of the build and the seconds the batches of the build and the verifier add
    let extra = |limit: u64| {
        let b = e.batches(limit).unwrap_or(0);
        let s = e.extra_seconds(b)
            + if verify {
                e.verify_extra_seconds(limit)
            } else {
                0.0
            };
        format!("{b} (+{s:.1} s)")
    };
    let recommended = hi.min(free);
    Ok(Question {
        name: "temp disk",
        min: lo,
        recommended,
        max: hi,
        free,
        free_means,
        extra: Some([extra(lo), extra(recommended), extra(hi)]),
        preset,
    })
}
