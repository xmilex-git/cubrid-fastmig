//! The `cubrid-fastmig` command: dispatches to one module per subcommand.
//!
//! Input: the command line. Output: the subcommand's effect; exit code 0 on success, 2 on usage errors, 1 on
//! failures.
//! Source: decision G1 workspace#481 (commands plan, migrate, verify, finalize). `migrate` and `verify` exist so far.
//!
//! Names follow GLOSSARY.md terms (AGENTS.md § Files and modules).

mod args;
mod heap_counter;
mod limits;
mod migrate;
mod partition_streams;
mod progress_args;
mod template_extract;
mod verify;

use std::process::ExitCode;

const USAGE: &str = "usage: cubrid-fastmig migrate <db> --dest <dir> [options]   (cubrid-fastmig migrate --help)\n       cubrid-fastmig verify <db> [options]                  (cubrid-fastmig verify --help)";

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let result = match argv.first().map(String::as_str) {
        Some("migrate") => migrate::run(&argv[1..]),
        Some("template-extract") => template_extract::run(&argv[1..]),
        Some("partition-streams") => partition_streams::run(&argv[1..]),
        // verify has its own exit codes: 1 = migration defects, 2 = usage or tool error (G3 workspace#483)
        Some("verify") => {
            return match verify::run(&argv[1..]) {
                Ok(code) => ExitCode::from(code),
                Err(args::CliError::Usage(m)) => {
                    eprintln!("error: {m}\n{USAGE}");
                    ExitCode::from(2)
                }
                Err(args::CliError::Failed(e)) => {
                    eprintln!("error: {e}");
                    ExitCode::from(2)
                }
            };
        }
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
