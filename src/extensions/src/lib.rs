//! zvcs — the git-shadowing superset engine, as a library.
//!
//! The `git` binary (`src/main.rs`) is a thin entry point over [`run`]. Exposing
//! the engine as a library lets integration tests drive the coordination layer
//! (e.g. [`lock::RepoLock`] against a live `zdaemon`) directly.

pub mod dispatch;
pub mod lock;
pub mod porcelain;
pub mod superset;

use std::process::ExitCode;

/// Parse `argv`, dispatch the subcommand, and return the process exit code.
/// Errors are reported terse on stderr as `zvcs: <command>: <reason>`.
pub fn run() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(sub) = args.first() else {
        eprintln!("zvcs: no subcommand given");
        return ExitCode::FAILURE;
    };
    let rest = &args[1..];
    match dispatch::run(sub, rest) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("zvcs: {sub}: {e:#}");
            ExitCode::FAILURE
        }
    }
}
