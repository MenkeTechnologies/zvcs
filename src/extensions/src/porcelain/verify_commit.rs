//! `git verify-commit` — check the GPG signature of commit objects.
//!
//! Covered, byte-identically with stock git: option parsing (`-v`/`--verbose`,
//! `--raw`, their `--no-` forms, `-h`, `--`), the usage block and its exit code
//! 129, and every verdict that can be reached *without* running a signature
//! checker:
//!   * an unresolvable spec        → `error: commit '<name>' not found.`
//!   * an oid with no object       → `error: <name>: unable to read file.`
//!   * a non-commit object         → `error: <name>: cannot verify a non-commit
//!                                    object of type <type>.`
//!   * a commit carrying no `gpgsig` header → no output at all, exit 1
//!     (git's `check_commit_signature` fails before it ever spawns gpg, so `-v`
//!     prints nothing on this path either — verified against git 2.55.0)
//! Like git, each `<commit>` is processed in order, errors do not stop the loop,
//! and the process exits 1 if any of them failed.
//!
//! NOT covered: the actual cryptographic verdict for a commit that *is* signed.
//! That needs git's `gpg-interface` substrate — spawning `gpg`/`gpgsm`/
//! `ssh-keygen -Y` per `gpg.format`/`gpg.program`, parsing `--status-fd` lines
//! into a trust result, and relaying the checker's own stderr verbatim — none of
//! which exists in the vendored crates. gitoxide can only *extract* the
//! signature and its payload; `gix::Commit::signature()` carries an upstream
//! `TODO: make it possible to verify the signature` at
//! `src/ported/gix/src/object/commit.rs:215`. A signed commit therefore fails
//! with a precise message rather than inventing a verdict, because guessing
//! "good" here would be indistinguishable from a real verification pass.
//!
//! Exit codes follow git rather than the caller's generic failure path: usage
//! errors (including `-h`) exit 129, a failed verification exits 1.

use anyhow::{bail, Result};
use std::process::ExitCode;

/// git's own usage block, printed on stderr next to `error: unknown …` and on
/// stdout for `-h`.
const USAGE: &str = "\
usage: git verify-commit [-v | --verbose] [--raw] <commit>...

    -v, --[no-]verbose    print commit contents
    --[no-]raw            print raw gpg status output
";

/// `git verify-commit` — validate the signature made by `git commit -S`.
///
/// Argument handling mirrors `builtin/verify-commit.c`: options and commit
/// specs may interleave, `--` ends option parsing, and an empty positional list
/// is a usage error. Specs are resolved *without* peeling, matching git's
/// `repo_get_oid` — so an annotated tag is reported as "a non-commit object of
/// type tag" instead of quietly resolving to the commit underneath it.
pub fn verify_commit(args: &[String]) -> Result<ExitCode> {
    // Dispatch passes the subcommand itself at index 0.
    let args = match args.first() {
        Some(a) if a == "verify-commit" => &args[1..],
        _ => args,
    };

    let mut verbose = false;
    let mut raw = false;
    let mut names: Vec<&str> = Vec::new();
    let mut no_more_opts = false;

    for a in args {
        let a = a.as_str();

        // A bare `-` is a positional to `parse_options`, not an option.
        if no_more_opts || a == "-" || !a.starts_with('-') {
            names.push(a);
            continue;
        }

        if let Some(long) = a.strip_prefix("--") {
            match long {
                "" => no_more_opts = true,
                "verbose" => verbose = true,
                "no-verbose" => verbose = false,
                "raw" => raw = true,
                "no-raw" => raw = false,
                _ => {
                    eprintln!("error: unknown option `{long}'");
                    eprintln!("{USAGE}");
                    return Ok(ExitCode::from(129));
                }
            }
            continue;
        }

        // Grouped short flags, e.g. `-vv`. None of them take a value.
        for c in a[1..].chars() {
            match c {
                'v' => verbose = true,
                // `-h` short-circuits before anything else, repo included.
                'h' => {
                    println!("{USAGE}");
                    return Ok(ExitCode::from(129));
                }
                _ => {
                    eprintln!("error: unknown switch `{c}'");
                    eprintln!("{USAGE}");
                    return Ok(ExitCode::from(129));
                }
            }
        }
    }

    if names.is_empty() {
        eprintln!("{USAGE}");
        return Ok(ExitCode::from(129));
    }

    // git runs the builtin under RUN_SETUP, so a missing repository is fatal
    // before any commit is looked at.
    let repo = gix::discover(".")?;

    let mut had_error = false;
    for name in names {
        if !verify_one(&repo, name, verbose, raw)? {
            had_error = true;
        }
    }

    Ok(if had_error {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

/// Verify a single `<commit>` spec, returning `false` when git would have
/// counted it as an error. Diagnostics go to stderr in git's exact wording.
///
/// Returns `Err` only for the one case this port cannot decide: a commit that
/// actually carries a signature.
fn verify_one(repo: &gix::Repository, name: &str, verbose: bool, raw: bool) -> Result<bool> {
    let Ok(id) = repo.rev_parse_single(name) else {
        eprintln!("error: commit '{name}' not found.");
        return Ok(false);
    };

    // `repo_get_oid` does not prove the object exists; `parse_object` does.
    let Ok(header) = repo.find_header(id) else {
        eprintln!("error: {name}: unable to read file.");
        return Ok(false);
    };

    let kind = header.kind();
    if !kind.is_commit() {
        eprintln!("error: {name}: cannot verify a non-commit object of type {kind}.");
        return Ok(false);
    }

    let commit = repo.find_object(id)?.try_into_commit()?;

    // No `gpgsig` header: `check_commit_signature` bails out of
    // `parse_signed_commit` before setting a payload, so git emits nothing at
    // all — not even under `-v` — and just fails.
    if commit.signature()?.is_none() {
        return Ok(false);
    }

    // `-v` (print the payload to stdout) and `--raw` (print the checker's raw
    // status instead of its human output) only take effect once a checker has
    // run, so they are parsed and accepted but never reached.
    let _ = (verbose, raw);
    bail!(
        "{name}: commit is signed, but signature verification is not ported \
         (the vendored crates have no gpg-interface: no gpg/gpgsm/ssh-keygen \
         driver, no gpg.format/gpg.program handling, no --status-fd parsing; \
         gix Commit::signature() only extracts the signature)"
    )
}
