//! The `cubrid-fastmig` command: dispatches to one module per subcommand.
//!
//! Input: the command line. Output: the subcommand's effect; exit code 0 on success, 2 on usage errors, 1 on
//! failures, 3 when migrate or post finished but the required SQL or the engine check failed (workspace#611). `-h`/`--help` prints the command's whole help on stdout and exits 0; a usage error (a missing required
//! argument, an unknown option) prints one line saying what is wrong and the command's whole help on stderr, exit 2.
//! Source: decision G1 workspace#481 (commands plan, migrate, check, finalize). Plus the check `set-comparison` (ADR
//! 0007). Help and exit codes: G13 workspace#604.
//!
//! Names follow GLOSSARY.md terms (AGENTS.md § Files and modules).

mod args;
mod catalog_regeneration;
mod check;
mod class_live_rows;
mod csql_output;
mod database_files;
mod end_screen;
mod engine_check;
mod engine_sa;
mod finalize;
mod heap_counter;
mod limits;
mod local_datetime;
mod migrate;
mod partition_streams;
mod plan;
mod plan_report;
mod plan_screen;
mod post;
mod progress_args;
mod question_screen;
mod replace_target;
mod required_sql;
mod required_sql_run;
mod resource_estimate;
mod run_paths;
mod set_comparison;
mod source_fingerprint;
mod source_unload_copy;
mod storage_kind_detect;
mod target_conf_defaults;
mod target_dirs;
mod target_install_version;
mod target_space;
mod temp_disk_choice;
mod template_extract;
mod unload_comparison;
mod unload_files;
mod unload_normalize;
mod unload_rules_10_2;
mod unload_rules_before_11_4;
mod unload_rules_official;
mod volume_dirs;
mod volume_targets;
mod write_probe;

use std::process::ExitCode;

const USAGE: &str = "usage: cubrid-fastmig plan <db> [<new-db>] -F <dir> [options]      (cubrid-fastmig plan -h)\n       cubrid-fastmig migrate <db> [<new-db>] -F <dir> [options]   (cubrid-fastmig migrate -h)\n       cubrid-fastmig check <db> [options]                          (cubrid-fastmig check -h)\n       cubrid-fastmig set-comparison <source-prefix> <target-prefix>   (cubrid-fastmig set-comparison -h)\n       cubrid-fastmig post <target-dir> [options]                   (cubrid-fastmig post -h)\n       cubrid-fastmig finalize <target-dir>                         (cubrid-fastmig finalize -h)";

unsafe extern "C" {
    fn mallopt(param: i32, value: i32) -> i32;
}

/// glibc `M_MMAP_THRESHOLD`.
const M_MMAP_THRESHOLD: i32 = -3;

/// The whole help of a user command.
fn help_of(cmd: &str) -> Option<String> {
    Some(match cmd {
        "plan" => format!("{}\n{}", plan::HELP, progress_args::STYLE_HELP),
        "migrate" => format!("{}\n{}", migrate::HELP, progress_args::HELP),
        "check" => format!("{}\n{}", check::HELP, progress_args::STYLE_HELP),
        "finalize" => finalize::HELP.to_string(),
        "post" => post::HELP.to_string(),
        "set-comparison" => set_comparison::HELP.to_string(),
        _ => return None,
    })
}

fn main() -> ExitCode {
    // blocks of 1 MiB or more (key runs, merge and copy buffers) are mapped and go back to the system when freed:
    // glibc raises its threshold after the first such free, and later ones stayed in the per-thread arenas (the big
    // develop DB, --temp-mem-size 1G --temp-disk-size min: max RSS 220 MiB above the live heap; workspace#526)
    // SAFETY: called once before any other thread exists; an unknown parameter only returns 0
    unsafe { mallopt(M_MMAP_THRESHOLD, 1 << 20) };
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = argv.first().map(String::as_str) else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let rest = &argv[1..];
    if matches!(cmd, "-h" | "--help") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let help = help_of(cmd);
    if let Some(h) = &help
        && rest.iter().any(|a| a == "-h" || a == "--help")
    {
        println!("{h}");
        return ExitCode::SUCCESS;
    }
    let help = help.unwrap_or_else(|| USAGE.to_string());
    let result = match cmd {
        // 3 = migrated, but the required SQL or the engine check failed (workspace#611)
        "migrate" => migrate::run(rest),
        "finalize" => finalize::run(rest).map(|()| 0),
        "template-extract" => template_extract::run(rest).map(|()| 0),
        "partition-streams" => partition_streams::run(rest).map(|()| 0),
        // check has its own exit codes: 1 = migration defects, 2 = usage or tool error (G3 workspace#483)
        "check" => return checker_exit(check::run(rest), &help),
        // 1 = the plan refuses the migration (G1 workspace#481)
        "plan" => return checker_exit(plan::run(rest), &help),
        // 1 = the two unload sets differ (ADR 0007)
        "set-comparison" => return checker_exit(set_comparison::run(rest), &help),
        // 3 = the required SQL or the engine check failed (workspace#611)
        "post" => return checker_exit(post::run(rest), &help),
        "verify" => {
            eprintln!("error: unknown command verify (renamed: use check)\n{USAGE}");
            return ExitCode::from(2);
        }
        _ => {
            eprintln!("error: unknown command {cmd}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(code) => ExitCode::from(code),
        Err(args::CliError::Usage(m)) => {
            eprintln!("error: {m}\n{help}");
            ExitCode::from(2)
        }
        Err(args::CliError::Failed(e)) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Exit code of a checking subcommand: its own code (0 = clean, 1 = findings), 2 on usage or tool error.
fn checker_exit(r: Result<u8, args::CliError>, help: &str) -> ExitCode {
    match r {
        Ok(code) => ExitCode::from(code),
        Err(args::CliError::Usage(m)) => {
            eprintln!("error: {m}\n{help}");
            ExitCode::from(2)
        }
        Err(args::CliError::Failed(e)) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}
