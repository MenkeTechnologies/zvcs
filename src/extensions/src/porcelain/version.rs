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
//!     on stderr with **no** usage block (it is `PARSE_OPT_ERROR`, not
//!     `PARSE_OPT_UNKNOWN`), exit 129, using the canonical option name
//!     `optname()` would print — `build-options` for the positive spelling and
//!     `no-build-options` for the negated one, however far either was
//!     abbreviated.
//!   * `parse_options` long-option abbreviation (`--b`, `--bu`, … all resolve to
//!     `--build-options`) and `--no-`-prefixed negation, including negation of
//!     an abbreviation (`--no-b`). `--no-build-options` leaves the flag clear,
//!     so it prints exactly the plain-`git version` output. Because the sole
//!     entry is negatable, `starts_with("no-", arg)` makes a bare `--n` and
//!     `--no` name its negation, while `--no-` names it twice over and is the
//!     `ambiguous option:` refusal — error on stderr, block on stdout, 129.
//!
//!   * `--build-options` — [`get_version_info`], reduced to the lines that are
//!     true of *this* binary. Each of git's lines is either reported honestly or
//!     left out; none is copied from stock. See [`get_version_info`] for the
//!     line-by-line reasoning. `git diagnose` and `git bugreport` embed the same
//!     block through the same function, exactly as `cmd_diagnose()` embeds
//!     `get_version_info(&buf, 1)`, so the three can never disagree.
//!
//! The build report will not match stock's byte for byte, and is not meant to:
//! it describes a Rust binary on gitoxide, not a C binary on zlib/libcurl. The
//! values that differ do so because they are *true here and false there*
//! (`rust: enabled`, `SHA-1: sha1-checked`), and the lines that are absent are
//! absent because this build has no such component to report — the same reason
//! git's own `#ifdef`s drop `libcurl:` from a build without curl.
//!
//! Note that `git <cmd> --help` never reaches a builtin: `git.c` rewrites it to
//! `git help <cmd>` before dispatch, so `--help` is not handled here.

use anyhow::Result;
use std::process::{Command, ExitCode};

use super::{resolve_long, Arg, LongOpt, Resolved};

/// The git version this port reproduces, as printed by `git version`.
///
/// `diagnose.rs` heads its report with the same string; `git version` and
/// `git diagnose` must never disagree about what this binary claims to be.
pub(crate) const GIT_VERSION: &str = "2.55.0";

/// `usage_with_options()` rendering of `builtin/version.c`'s option table,
/// verbatim (including the blank line before the option list and the trailing
/// blank line).
const USAGE: &str = concat!(
    "usage: git version [--build-options]\n",
    "\n",
    "    --[no-]build-options  also print build options\n",
    "\n",
);

/// `cmd_version`'s `struct option options[]` (help.c:841-844), which holds one
/// `OPT_BOOL` and nothing else.
const LONG_OPTS: &[LongOpt] = &[LongOpt {
    name: "build-options",
    neg: true,
    arg: Arg::None,
}];

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

        // parse_options_step() tests `--help-all` with a `strcmp()` of its own,
        // after the `--` break above and ahead of parse_long_opt(): the name
        // never abbreviates and never takes an `=<value>`, so it never reaches
        // `match_long()`. `cmd_version`'s table holds only [`OPT`] and no
        // `PARSE_OPT_HIDDEN` entry, so `USAGE_FULL` is the block `-h` prints.
        if a == "--help-all" {
            print!("{USAGE}");
            return Ok(ExitCode::from(129));
        }

        if let Some(long) = a.strip_prefix("--") {
            let negated = match resolve_long(LONG_OPTS, long) {
                Resolved::One(_, sense) => sense,
                Resolved::Ambiguous(first, second) => {
                    return Ok(super::ambiguous_option(a, &first, &second, USAGE))
                }
                Resolved::Unknown => {
                    return Ok(usage_error(&format!("unknown option `{long}'")))
                }
            };
            if long.contains('=') {
                // `optname()` spells the entry the way this sense of it parses,
                // however far it was abbreviated: `--no-b=x` names
                // `no-build-options`.
                let canonical = match negated {
                    true => "no-build-options",
                    false => "build-options",
                };
                // `get_value()` returns `PARSE_OPT_ERROR` here, which prints its
                // own line and *no* usage block.
                eprintln!("error: option `{canonical}' takes no value");
                return Ok(ExitCode::from(129));
            }
            build_options = !negated;
        } else {
            // Short flags, grouped as git allows (`-hh`). Only `-h` exists.
            if let Some(c) = a[1..].chars().next() {
                if c != 'h' {
                    return Ok(usage_error(&format!("unknown switch `{c}'")));
                }
                // git's `-h` path prints usage on stdout and exits 129.
                print!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
        }
    }

    // `cmd_version`: build the whole report into one buffer, then print it.
    let mut buf = String::new();
    get_version_info(&mut buf, build_options);
    print!("{buf}");
    Ok(ExitCode::SUCCESS)
}

/// The shell this binary runs every shell child through — hooks, `!`-aliases,
/// clean/smudge and textconv filters, the pager, editors, mergetool/difftool,
/// `filter-branch`, `submodule foreach`, `rebase --exec`.
///
/// Like git's `SHELL_PATH`, it is one absolute path fixed in the source rather
/// than a name resolved on `PATH`, and [`crate::external::SHELL_PATH`] is the
/// single definition every one of those call sites reaches for. Printing it here
/// is therefore a true statement about what the binary will execute.
const SHELL_PATH: &str = crate::external::SHELL_PATH;

/// The SHA-1 implementation this build links: the `sha1-checked` crate, which
/// `gix-hash` drives with the same collision-detection algorithm git uses.
///
/// git's `SHA1_BACKEND` names a C library (`SHA1_DC`, `BLK_SHA1`,
/// `OPENSSL_SHA1`, …). None of those is linked here, so the crate is named
/// instead; printing git's token would claim a C dependency that does not exist.
const SHA1_BACKEND: &str = "sha1-checked";

/// Port of `get_version_info()` (help.c), reduced to what is true of this
/// binary. `git version --build-options`, `git diagnose` and `git bugreport` all
/// render their version block through here, as they all call this function in
/// git.
///
/// Reported, because each is a real property of *this* build:
///   * `cpu:` — `GIT_HOST_CPU`, see [`host_cpu`].
///   * the no-build-commit line — git's `git_built_from_commit_string[0] == 0`
///     branch. Nothing embeds a commit in this binary, so that branch is the
///     honest one.
///   * `sizeof-long:` / `sizeof-size_t:` — the C `long` and `size_t` widths of
///     the target this was compiled for, taken from `libc::c_long` and `usize`.
///   * `shell-path:` — [`SHELL_PATH`].
///   * `rust:` — git's `WITH_RUST`, "this build contains Rust code". It does.
///   * `SHA-1:` — [`SHA1_BACKEND`].
///   * `default-ref-format:` / `default-hash:` — `files` and `sha1`. Both are
///     the defaults here *and* the only formats this port accepts;
///     `porcelain::init` rejects `--ref-format=reftable` and
///     `--object-format=sha256` for want of a backend.
///
/// Omitted, because this build has no such component — the same reason git's own
/// `#ifdef`s drop a line:
///   * `feature: fsmonitor--daemon` — git prints it when
///     `fsmonitor_ipc__is_supported()`. Only the *client* half is ported
///     (`porcelain::fsmonitor__daemon`); there is no IPC server to talk to.
///   * `gettext:` — no message catalogs are compiled in.
///   * `libcurl:` / `OpenSSL:` — the transport is reqwest + rustls, a pure-Rust
///     stack; neither C library is linked.
///   * `zlib:` / `zlib-ng:` — git prints the `ZLIB_VERSION` of the library it
///     links. This build links no zlib: `gix-zlib`'s deflate is an in-tree
///     transcription of zlib's `deflate.c`/`trees.c` and its inflate is
///     `zlib-rs`. Neither carries a `ZLIB_VERSION`, and quoting one would name a
///     library that is not there.
///   * `SHA-256:` — git prints it unconditionally because it always has a
///     backend. The `sha2` crate is not in this build's dependency graph, so
///     there is nothing to name.
pub(crate) fn get_version_info(out: &mut String, show_build_options: bool) {
    // "The format of this string should be kept stable for compatibility with
    // external projects that rely on the output of `git version`."
    out.push_str(&format!("git version {GIT_VERSION}\n"));

    if !show_build_options {
        return;
    }
    out.push_str(&format!("cpu: {}\n", host_cpu()));
    out.push_str("no commit associated with this build\n");
    out.push_str(&format!(
        "sizeof-long: {}\n",
        std::mem::size_of::<libc::c_long>()
    ));
    out.push_str(&format!("sizeof-size_t: {}\n", std::mem::size_of::<usize>()));
    out.push_str(&format!("shell-path: {SHELL_PATH}\n"));
    out.push_str("rust: enabled\n");
    out.push_str(&format!("SHA-1: {SHA1_BACKEND}\n"));
    out.push_str("default-ref-format: files\n");
    out.push_str("default-hash: sha1\n");
}

/// `GIT_HOST_CPU`, which the Makefile defines as
/// `$(firstword $(subst -, ,$(uname_M)))` — `uname -m` truncated at its first
/// `-`. This is a fact about the host, not about how a C binary was compiled, so
/// it is reportable; `std::env::consts::ARCH` is not the same thing (it is the
/// Rust target arch, and prints `aarch64` where `uname -m` and stock git both
/// say `arm64`).
fn host_cpu() -> String {
    match Command::new("uname").arg("-m").output() {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout);
            let machine = text.trim();
            let first = machine.split('-').next().unwrap_or(machine);
            if first.is_empty() {
                std::env::consts::ARCH.to_string()
            } else {
                first.to_string()
            }
        }
        // Without `uname` there is nothing better to say than the target arch.
        _ => std::env::consts::ARCH.to_string(),
    }
}

/// Stock's usage-error path: `error: <msg>` followed by the usage block, both
/// on stderr, exit status 129.
fn usage_error(msg: &str) -> ExitCode {
    eprint!("error: {msg}\n{USAGE}");
    ExitCode::from(129)
}
