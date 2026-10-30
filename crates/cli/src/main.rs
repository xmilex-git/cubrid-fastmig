//! The `cubrid-fastmig` command: dispatches to one module per subcommand.
//!
//! Input: the command line. Output: the subcommand's effect; exit code 0 on success, 2 on usage errors, 1 on
//! failures.
//! Source: decision G1 workspace#481 (commands plan, migrate, verify, finalize). Plus the check `set-comparison` (ADR 0007).
//!
//! Names follow GLOSSARY.md terms (AGENTS.md § Files and modules).

mod args;
mod catalog_regeneration;
mod finalize;
mod heap_counter;
mod limits;
mod local_datetime;
mod migrate;
mod partition_streams;
mod plan;
mod plan_report;
mod progress_args;
mod resource_estimate;
mod set_comparison;
mod source_fingerprint;
mod target_space;
mod temp_disk_choice;
mod template_extract;
mod unload_files;
mod unload_normalize;
mod unload_rules_10_2;
mod unload_rules_before_11_4;
mod verify;
mod volume_dirs;

use std::process::ExitCode;

const USAGE: &str = "usage: cubrid-fastmig plan <db> --dest <dir> [options]      (cubrid-fastmig plan --help)\n       cubrid-fastmig migrate <db> --dest <dir> [options]   (cubrid-fastmig migrate --help)\n       cubrid-fastmig verify <db> [options]                  (cubrid-fastmig verify --help)\n       cubrid-fastmig set-comparison <source-prefix> <target-prefix>   (cubrid-fastmig set-comparison --help)\n       cubrid-fastmig finalize <target-dir>                  (cubrid-fastmig finalize --help)";

unsafe extern "C" {
    fn mallopt(param: i32, value: i32) -> i32;
}

/// glibc `M_MMAP_THRESHOLD`.
const M_MMAP_THRESHOLD: i32 = -3;

fn main() -> ExitCode {
    // blocks of 1 MiB or more (key runs, merge and copy buffers) are mapped and go back to the system when freed:
    // glibc raises its threshold after the first such free, and later ones stayed in the per-thread arenas (the big
    // develop DB, --mem 1G --temp-disk min: max RSS 220 MiB above the live heap, over --mem; workspace#526)
    // SAFETY: called once before any other thread exists; an unknown parameter only returns 0
    unsafe { mallopt(M_MMAP_THRESHOLD, 1 << 20) };
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let result = match argv.first().map(String::as_str) {
        Some("migrate") => migrate::run(&argv[1..]),
        Some("finalize") => finalize::run(&argv[1..]),
        Some("template-extract") => template_extract::run(&argv[1..]),
        Some("partition-streams") => partition_streams::run(&argv[1..]),
        // verify has its own exit codes: 1 = migration defects, 2 = usage or tool error (G3 workspace#483)
        Some("verify") => return checker_exit(verify::run(&argv[1..])),
        // 1 = the plan refuses the migration (G1 workspace#481)
        Some("plan") => return checker_exit(plan::run(&argv[1..])),
        // 1 = the two unload sets differ (ADR 0007)
        Some("set-comparison") => return checker_exit(set_comparison::run(&argv[1..])),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(args::CliError::Usage(m)) => {
            eprintln!("error: {m}\n{USAGE}");
            ExitCode::from(2)
        }
        Err(args::CliError::Failed(e)) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Exit code of a checking subcommand: its own code (0 = clean, 1 = findings), 2 on usage or tool error.
fn checker_exit(r: Result<u8, args::CliError>) -> ExitCode {
    match r {
        Ok(code) => ExitCode::from(code),
        Err(args::CliError::Usage(m)) => {
            eprintln!("error: {m}\n{USAGE}");
            ExitCode::from(2)
        }
        Err(args::CliError::Failed(e)) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}
