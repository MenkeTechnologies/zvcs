//! zvcs — the `git` shadow binary.
//!
//! A pure-Rust, git-compatible VCS built on the vendored gitoxide crates in
//! `src/ported`. It shadows stock `git` on PATH and serves every subcommand
//! natively; there is no fork/exec of upstream git. All logic lives in the
//! `zvcs` library crate — this binary is only the entry point.

fn main() -> std::process::ExitCode {
    zvcs::run()
}
