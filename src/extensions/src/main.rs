//! zvcs — the `git` shadow binary.
//!
//! A pure-Rust, git-compatible VCS built on the vendored gitoxide crates in
//! `src/ported`. It shadows stock `git` on PATH and serves every subcommand
//! natively; there is no fork/exec of upstream git. On top of git compatibility
//! it adds the zvcs "superset": coordination verbs that stock git cannot have
//! (queue/barrier serialization, background reconcile-to-main, forward-only
//! submodule pointer bumps).

mod dispatch;
mod porcelain;
mod superset;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(sub) = args.first() else {
        eprintln!("zvcs: no subcommand given");
        return ExitCode::FAILURE;
    };
    let rest = &args[1..];
    match dispatch::run(sub, rest) {
        Ok(code) => code,
        // zsh-compatible terse error: `zvcs: <command>: <reason>`
        Err(e) => {
            eprintln!("zvcs: {sub}: {e:#}");
            ExitCode::FAILURE
        }
    }
}
