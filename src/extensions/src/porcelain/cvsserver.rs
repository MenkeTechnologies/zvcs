//! `git cvsserver` — CVS server emulator.
//!
//! Stock `git-cvsserver` is a ~4700-line Perl script
//! (`git-cvsserver.perl`). It speaks the CVS wire protocol over stdin/stdout
//! and maintains a per-head SQLite revision database so that CVS clients see
//! stable `1.N` revision numbers across git history. It has no C
//! implementation and no plumbing equivalent.
//!
//! What is ported here (byte-identical to stock, verified against git 2.55.0):
//!   * The `Getopt::Long` front end at `git-cvsserver.perl:111-137` — the exact
//!     usage text, unique-prefix and case-insensitive long-option matching,
//!     `--version`/`-V` output, and the three `warn` + `die $usage` failure
//!     paths (`Unknown option: X`, `Option X requires an argument`,
//!     `Option X does not take an argument`), all on stderr with exit 255.
//!
//! What is NOT ported — the server itself. Everything past option parsing
//! (`pserver` authentication against `gitcvs.authdb`, the `Root`/`Directory`/
//! `Entry`/`Modified` request loop, `co`/`update`/`diff`/`status`/`log`/`add`/
//! `remove`/`ci` handlers, CVS revision numbering, and the `-kb` guessing) is
//! bailed on rather than approximated. The missing substrate is concrete and
//! not a matter of effort:
//!   * There is no CVS protocol layer anywhere in the vendored gitoxide — no
//!     crate implements the client or server side of the CVS request/response
//!     wire format.
//!   * The revision database is a `DBD::SQLite` schema
//!     (`gitcvs.<module>.sqlite`, configurable via `gitcvs.dbDriver`/`dbName`/
//!     `dbTableNamePrefix`) whose table layout and incremental-update
//!     semantics are defined only by the Perl script; reproducing it is a
//!     port of that script, not of any git C code.
//!   * Existing CVS sandboxes on disk depend on those exact revision numbers,
//!     so a plausible-looking reimplementation is worse than none: it would
//!     silently corrupt working copies rather than fail loudly.
//!
//! `-h`/`-H` also bail: in stock git they are a no-op. `@opts` declares
//! `'h|H'`, which populates `$state->{h}`, but the guard at
//! `git-cvsserver.perl:134` tests `$state->{help}` — so `-h` prints nothing and
//! falls straight into the server loop. Reproducing that means reproducing the
//! server loop.

use anyhow::{bail, Result};
use std::io::Write;
use std::process::ExitCode;

/// The git version this port reproduces, as printed by `--version`.
const GIT_VERSION: &str = "2.55.0";

/// `$usage` from `git-cvsserver.perl:111-123`, verbatim.
const USAGE: &str = concat!(
    "usage: git cvsserver [options] [pserver|server] [<directory> ...]\n",
    "    --base-path <path>  : Prepend to requested CVSROOT\n",
    "                          Can be read from GIT_CVSSERVER_BASE_PATH\n",
    "    --strict-paths      : Don't allow recursing into subdirectories\n",
    "    --export-all        : Don't check for gitcvs.enabled in config\n",
    "    --version, -V       : Print version information and exit\n",
    "    -h, -H              : Print usage information and exit\n",
    "\n",
    "<directory> ... is a list of allowed directories. If no directories\n",
    "are given, all are allowed. This is an additional restriction, gitcvs\n",
    "access still needs to be enabled by the gitcvs.enabled config option.\n",
    "Alternately, one directory may be specified in GIT_CVSSERVER_ROOT.\n",
);

/// The `@opts` specification at `git-cvsserver.perl:125-126`, as
/// `(canonical-name, aliases, takes-an-argument)`.
///
/// The canonical name is what `Getopt::Long` reports in its diagnostics;
/// aliases participate in matching but never in messages. Matching is
/// case-insensitive and accepts any unambiguous prefix, which is
/// `Getopt::Long`'s default (`ignore_case` + `auto_abbrev`).
const OPTS: &[(&str, &[&str], bool)] = &[
    ("h", &["H"], false),
    ("version", &["V"], false),
    ("base-path", &[], true),
    ("strict-paths", &[], false),
    ("export-all", &[], false),
];

/// Outcome of resolving one token against [`OPTS`].
enum Match {
    /// Index into [`OPTS`].
    One(usize),
    /// No name matched — `Unknown option: <as-written>`.
    None,
    /// A prefix matched more than one canonical name.
    Ambiguous,
}

/// Resolve `name` (dashes already stripped, `=value` already split off)
/// against [`OPTS`] the way `Getopt::Long` does by default: exact
/// case-insensitive hit on a name or alias first, otherwise a unique
/// case-insensitive prefix of a canonical name.
fn resolve(name: &str) -> Match {
    let lower = name.to_ascii_lowercase();

    for (i, (canonical, aliases, _)) in OPTS.iter().enumerate() {
        if *canonical == lower || aliases.iter().any(|a| a.to_ascii_lowercase() == lower) {
            return Match::One(i);
        }
    }

    let mut hit = None;
    for (i, (canonical, _, _)) in OPTS.iter().enumerate() {
        if canonical.starts_with(&lower) {
            if hit.is_some() {
                return Match::Ambiguous;
            }
            hit = Some(i);
        }
    }
    hit.map_or(Match::None, Match::One)
}

/// `warn`s the collected diagnostics, then `die $usage` — all on stderr,
/// exit 255 (perl's `die` status when `$!` and `$?` are both clear).
fn die(errors: &[String]) -> ExitCode {
    let mut err = std::io::stderr().lock();
    for e in errors {
        let _ = writeln!(err, "{e}");
    }
    let _ = write!(err, "{USAGE}");
    ExitCode::from(255)
}

/// `git cvsserver` — see the module documentation for the ported surface.
pub fn cvsserver(args: &[String]) -> Result<ExitCode> {
    // Getopt::Long collects every diagnostic before failing once, so two bad
    // options produce two `Unknown option:` lines above a single usage block.
    let mut errors: Vec<String> = Vec::new();
    let mut want_version = false;
    // Any option that only matters to the server loop; recorded so the bail
    // below can name the flag that was actually asked for.
    let mut server_flag: Option<String> = None;
    let mut operands: Vec<&str> = Vec::new();

    let mut it = args.iter().peekable();
    let mut no_more_opts = false;
    while let Some(arg) = it.next() {
        if no_more_opts || !arg.starts_with('-') || arg == "-" {
            operands.push(arg);
            continue;
        }
        if arg == "--" {
            no_more_opts = true;
            continue;
        }

        let body = arg.trim_start_matches('-');
        let (name, inline) = match body.split_once('=') {
            Some((n, v)) => (n, Some(v)),
            None => (body, None),
        };

        match resolve(name) {
            Match::None | Match::Ambiguous => errors.push(format!("Unknown option: {name}")),
            Match::One(i) => {
                let (canonical, _, takes_arg) = OPTS[i];
                if !takes_arg && inline.is_some() {
                    errors.push(format!("Option {canonical} does not take an argument"));
                    continue;
                }
                if takes_arg && inline.is_none() && it.peek().is_none() {
                    errors.push(format!("Option {canonical} requires an argument"));
                    continue;
                }
                if takes_arg && inline.is_none() {
                    it.next();
                }
                match canonical {
                    "version" => want_version = true,
                    other => server_flag = Some(other.to_string()),
                }
            }
        }
    }

    if !errors.is_empty() {
        return Ok(die(&errors));
    }

    // `git-cvsserver.perl:130-133`: --version wins over everything that follows.
    if want_version {
        println!("git-cvsserver version {GIT_VERSION}");
        return Ok(ExitCode::SUCCESS);
    }

    if let Some(flag) = server_flag {
        bail!(
            "unsupported flag {flag:?} (ported: --version/-V and the option-parsing \
             failure paths only; every other flag feeds the CVS protocol server, which \
             has no gitoxide substrate — no CVS wire-protocol implementation and no \
             gitcvs.*.sqlite revision database)"
        );
    }

    bail!(
        "unsupported: the CVS protocol server is not ported (ported: --version/-V and \
         the option-parsing failure paths). Stock git-cvsserver is a Perl script that \
         speaks the CVS request/response protocol on stdin/stdout and keeps CVS revision \
         numbers in a DBD::SQLite database; the vendored gitoxide implements neither, and \
         an approximation would silently corrupt existing CVS sandboxes"
    );
}
