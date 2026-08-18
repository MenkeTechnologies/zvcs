//! `git verify-commit` — check the GPG signature of commit objects.
//!
//! Covered, byte-identically with stock git: option parsing (`-v`/`--verbose`,
//! `--raw`, their `--no-` forms, `-h`, `--`), the usage block and its exit code
//! 129, and every verdict that can be reached *without* running a signature
//! checker:
//! ```text
//!   * an unresolvable spec        → `error: commit '<name>' not found.`
//!   * an oid with no object       → `error: <name>: unable to read file.`
//!   * a non-commit object         → `error: <name>: cannot verify a non-commit
//!                                    object of type <type>.`
//!   * a commit carrying no `gpgsig` header → no output at all, exit 1
//!     (git's `check_commit_signature` fails before it ever spawns gpg, so `-v`
//!     prints nothing on this path either — verified against git 2.55.0)
//! ```
//! Like git, each `<commit>` is processed in order, errors do not stop the loop,
//! and the process exits 1 if any of them failed.
//!
//! A commit that *is* signed goes through [`crate::gitsig`], which is this
//! crate's port of `gpg-interface.c`: `check_signature()` picks the backend off
//! the signature's own armor header, runs `gpg`/`gpgsm`/`ssh-keygen -Y` with
//! git's argument vector, and keeps both of the checker's streams. So `-v`
//! prints the payload, the plain form relays the checker's report and `--raw`
//! its `--status-fd` stream — all three verbatim, because they *are* the
//! checker's bytes and nothing here rewrites them. gitoxide's own
//! `gix::Commit::signature()` only extracts the signature (upstream `TODO: make
//! it possible to verify the signature`, `src/ported/gix/src/object/commit.rs:215`),
//! which is why the payload split and the verification both live in `gitsig`
//! rather than in the vendored crate.
//!
//! Exit codes follow git rather than the caller's generic failure path: usage
//! errors (including `-h`) exit 129, a failed verification exits 1.

use anyhow::Result;
use std::process::ExitCode;

/// git's own usage block, printed on stderr next to `error: unknown …` and on
/// stdout for `-h`.
/// `cmd_verify_commit()`'s `struct option verify_commit_options[]`
/// (builtin/verify-commit.c), in table order, as [`super::resolve_long`] reads it.
const LONG_OPTS: &[super::LongOpt] = &[
    super::LongOpt { name: "verbose",                     neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "raw",                         neg: true,  arg: super::Arg::None },
];

/// `usage_with_options()` over `builtin/verify-commit.c`'s option table. It ends
/// with the blank line the renderer emits after the last entry, so every site
/// writes it with `print!`/`eprint!` rather than adding a newline of its own —
/// which is what `-h` and the `ambiguous option:` block have to agree on.
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

        // parse_options_step() tests `--help-all` with a `strcmp()` of its own,
        // ahead of parse_long_opt(): the name never abbreviates and never takes
        // an `=<value>`, which is why it is absent from [`LONG_OPTS`] and
        // matched here rather than after resolution. verify-commit's table has
        // no `PARSE_OPT_HIDDEN` entry, so `USAGE_FULL` renders the same block
        // `-h` prints.
        if a == "--help-all" {
            print!("{USAGE}");
            return Ok(ExitCode::from(129));
        }

        // Respell a unique abbreviation as the name it resolves to, so an
        // abbreviation lands on the arm its full spelling lands on.
        let canonical;
        let a = match super::canonical_long(a, LONG_OPTS) {
            super::Long::Name(name) => {
                canonical = name;
                canonical.as_ref()
            }
            super::Long::Ambiguous(first, second) => {
                return Ok(super::ambiguous_option(a, &first, &second, USAGE))
            }
        };
        if let Some(long) = a.strip_prefix("--") {
            match long {
                "" => no_more_opts = true,
                "verbose" => verbose = true,
                "no-verbose" => verbose = false,
                "raw" => raw = true,
                "no-raw" => raw = false,
                _ => {
                    eprintln!("error: unknown option `{long}'");
                    eprint!("{USAGE}");
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
                    print!("{USAGE}");
                    return Ok(ExitCode::from(129));
                }
                _ => {
                    eprintln!("error: unknown switch `{c}'");
                    eprint!("{USAGE}");
                    return Ok(ExitCode::from(129));
                }
            }
        }
    }

    if names.is_empty() {
        eprint!("{USAGE}");
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

/// `gpg_interface_lazy_init()`: read the signing configuration and reject the two
/// values `git_gpg_config()` rejects, returning `configured_min_trust_level` — the
/// only one a verification then consults.
///
/// Both rejections are an `error()` inside the config reader followed by its own
/// `die()`, so what leaves here is a [`crate::fatal::Silent`] carrying nothing but
/// the exit code: the message is already on stderr, and this port's own voice
/// after it would double it. The `die()`'s second line, which names the file and
/// line the value came from, is not reproduced — gix's config parser does not
/// carry per-entry line numbers.
///
/// Calling this where a signature is about to be checked, rather than up front, is
/// not an optimization but the observable behaviour: `git status` and `git log` in
/// a repository with a bad `gpg.format` say nothing at all.
pub(crate) fn min_trust_level(repo: &gix::Repository) -> Result<crate::gitsig::Trust> {
    let reported = |key: &str, value: &str| {
        eprintln!("error: invalid value for '{key}': '{value}'");
        anyhow::Error::new(crate::fatal::Silent(crate::fatal::EXIT_FATAL))
    };
    crate::gitsig::validate_format(repo).map_err(|v| reported("gpg.format", &v))?;
    crate::gitsig::configured_min_trust_level(repo).map_err(|v| reported("gpg.mintrustlevel", &v))
}

/// Verify a single `<commit>` spec, returning `false` when git would have
/// counted it as an error. Diagnostics go to stderr in git's exact wording.
fn verify_one(repo: &gix::Repository, name: &str, verbose: bool, raw: bool) -> Result<bool> {
    // `repo_get_oid` is `get_oid_basic()`, whose first branch accepts a
    // full-length hex name as the id without asking the odb — so a well-formed
    // but absent id gets past this check and fails at `parse_object` below.
    let Some(id) = crate::objname::resolve(repo, name) else {
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

    // `verify_commit_buffer()`: the payload is the commit object with its
    // `gpgsig` block removed, which is what was signed. No such header at all is
    // `parse_buffer_signed_by_header() <= 0` — `sigc->payload` is never set, so
    // `print_signature_buffer` emits nothing (not even under `-v`) and the
    // verification simply fails.
    let Some((signature, payload)) = crate::gitsig::split_signed(&commit.data) else {
        return Ok(false);
    };

    // `gpg_interface_lazy_init()` is *lazy*: `gpg.minTrustLevel` is read by the
    // first `check_signature()`, not while config is loaded. So an unsigned
    // commit never reads it at all, and a repository with an unparseable value
    // still exits quietly at 1 there rather than reporting a config error — which
    // is what reading it up front got wrong.
    let min_trust = min_trust_level(repo)?;

    let sigc = crate::gitsig::verify_full(&signature, &payload);

    // `print_signature_buffer()` (gpg-interface.c:690): the payload under
    // `GPG_VERIFY_VERBOSE`, then the checker's own report — its `--status-fd`
    // stream under `--raw`, its human-readable output otherwise.
    if verbose {
        std::io::Write::write_all(&mut std::io::stdout(), &payload)?;
    }
    let shown = if raw { &sigc.gpg_status } else { &sigc.output };
    std::io::Write::write_all(&mut std::io::stderr(), shown)?;

    Ok(sigc.verified(min_trust))
}
