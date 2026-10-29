//! The `cubrid-fastmig` command: dispatches to one module per subcommand.
//!
//! Input: the command line. Output: the subcommand's effect; exit code 0 on success, 2 on usage errors, 1 on
//! failures.
//! Source: decision G1 workspace#481 (commands plan, migrate, verify, finalize). Only `migrate` exists so far.

mod args;
mod migrate;

use std::process::ExitCode;

const USAGE: &str =
    "usage: cubrid-fastmig migrate <db> --dest <dir> [options]   (cubrid-fastmig migrate --help)";

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let result = match argv.first().map(String::as_str) {
        Some("migrate") => migrate::run(&argv[1..]),
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
