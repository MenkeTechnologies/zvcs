//! `git version` — display version information about Git.
//!
//! Stock's implementation is `builtin/version.c`: a one-option `parse_options`
//! table followed by `printf("git version %s\n", git_version_string)`. The
//! version string it prints is a build-time constant, so this port carries it
//! as [`GIT_VERSION`], pinned to the same git release the rest of this crate
//! reproduces (`receive_pack.rs`, `cvsserver.rs` pin the same constant).
//!
//! Covered, byte-identically with stock git:
//!   * `git version` → `git version <GIT_VERSION>\n` on stdout, exit 0.
//!   * Trailing non-option arguments, and anything after `--`, are accepted and
//!     ignored — `cmd_version` never looks at the residual argv.
//!   * `-h` → the usage block on **stdout**, exit 129 (git's `-h` path uses
//!     stdout; only the error paths use stderr). `-h` wins wherever it appears.
//!   * Unknown long option → ``error: unknown option `<name>'`` plus the usage
//!     block on stderr, exit 129. Unknown short → ``error: unknown switch
//!     `<c>'``, same shape.
//!   * `--build-options=<v>` → ``error: option `build-options' takes no value``
//!     plus usage on stderr, exit 129, using the canonical option name (git
//!     reports `build-options` for the positive spelling and `no-build-options`
//!     for the negated one, regardless of how far either was abbreviated).
//!   * `parse_options` long-option abbreviation (`--b`, `--bu`, … all resolve to
//!     `--build-options`) and `--no-`-prefixed negation, including negation of
//!     an abbreviation (`--no-b`). `--no-build-options` leaves the flag clear,
//!     so it prints exactly the plain-`git version` output.
//!
//! Faithfully unsupported — `--build-options` `bail!`s rather than emitting
//! divergent output. Every line it prints is the C build configuration of the
//! *stock* binary: `cpu:` (`GIT_HOST_CPU`), the build commit, `sizeof-long` /
//! `sizeof-size_t`, `shell-path` (`SHELL_PATH`), the compiled-in `feature:`
//! list, `gettext:`, the linked `libcurl:` and `zlib:` versions, and the
//! `SHA-1:` / `SHA-256:` backend names. Those are properties of how a C program
//! was compiled and linked; a Rust binary built from gitoxide has no honest way
//! to report them, and guessing them would be fabrication. `diagnose.rs` records
//! the same conclusion for the report section that embeds this output.
//!
//! Note that `git <cmd> --help` never reaches a builtin: `git.c` rewrites it to
//! `git help <cmd>` before dispatch, so `--help` is not handled here.

use anyhow::{bail, Result};
use std::process::ExitCode;

/// The git version this port reproduces, as printed by `git version`.
const GIT_VERSION: &str = "2.55.0";

/// `usage_with_options()` rendering of `builtin/version.c`'s option table,
/// verbatim (including the blank line before the option list and the trailing
/// blank line).
const USAGE: &str = concat!(
    "usage: git version [--build-options]\n",
    "\n",
    "    --[no-]build-options  also print build options\n",
    "\n",
);

/// The sole long option, used both for abbreviation matching and for the
/// canonical name git names in its `takes no value` error.
const OPT: &str = "build-options";

/// `git version` — print the version string, optionally with build options.
pub fn version(args: &[String]) -> Result<ExitCode> {
    let mut build_options = false;
    let mut no_more_opts = false;

    for a in args {
        // Past `--`, and for any bare operand, `cmd_version` ignores the
        // residual argv entirely.
        if no_more_opts || a == "-" || !a.starts_with('-') {
            continue;
        }
        if a == "--" {
            no_more_opts = true;
            continue;
        }

        if let Some(long) = a.strip_prefix("--") {
            let (name, value) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (long, None),
            };
            let Some((negated, canonical)) = match_long(name) else {
                return Ok(usage_error(&format!("unknown option `{name}'")));
            };
            if value.is_some() {
                return Ok(usage_error(&format!(
                    "option `{canonical}' takes no value"
                )));
            }
            build_options = !negated;
        } else {
            // Short flags, grouped as git allows (`-hh`). Only `-h` exists.
            for c in a[1..].chars() {
                if c != 'h' {
                    return Ok(usage_error(&format!("unknown switch `{c}'")));
                }
                // git's `-h` path prints usage on stdout and exits 129.
                print!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
        }
    }

    if build_options {
        bail!(
            "--build-options is not supported: every line it prints (cpu, build commit, \
             sizeof-long/size_t, shell-path, feature list, gettext, libcurl, zlib, the \
             SHA-1/SHA-256 backend names) is the C build configuration of the stock binary, \
             which a Rust build from gitoxide cannot report honestly"
        );
    }

    println!("git version {GIT_VERSION}");
    Ok(ExitCode::SUCCESS)
}

/// Resolve a long-option spelling (already stripped of `--` and any `=value`).
///
/// Returns `(negated, canonical_name)`, where `canonical_name` is the spelling
/// git uses in its `takes no value` diagnostic. Mirrors `parse_options`'
/// unique-prefix abbreviation and its `no-` negation prefix, which composes with
/// abbreviation (`--no-b` is `--no-build-options`).
fn match_long(name: &str) -> Option<(bool, &'static str)> {
    if !name.is_empty() && OPT.starts_with(name) {
        return Some((false, OPT));
    }
    let rest = name.strip_prefix("no-")?;
    if !rest.is_empty() && OPT.starts_with(rest) {
        return Some((true, "no-build-options"));
    }
    None
}

/// Stock's usage-error path: `error: <msg>` followed by the usage block, both
/// on stderr, exit status 129.
fn usage_error(msg: &str) -> ExitCode {
    eprint!("error: {msg}\n{USAGE}");
    ExitCode::from(129)
}
