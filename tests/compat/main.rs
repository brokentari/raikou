//! Compatibility suite: compares Raikou with a real Redis server, the oracle.

mod cli;
mod oracle;

use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let environment = match oracle::Environment::start() {
        Ok(environment) => environment,
        Err(message) => {
            eprintln!("compat: {message}");
            return ExitCode::FAILURE;
        }
    };
    match cli::run_all(&environment, root) {
        Ok(0) => ExitCode::SUCCESS,
        Ok(failures) => {
            eprintln!("compat: {failures} mismatches");
            ExitCode::FAILURE
        }
        Err(message) => {
            eprintln!("compat: {message}");
            ExitCode::FAILURE
        }
    }
}
