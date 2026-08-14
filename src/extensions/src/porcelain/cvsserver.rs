//! `git cvsserver` — CVS server emulator.
//!
//! Stock `git-cvsserver` is a ~4700-line Perl script
//! (`git-cvsserver.perl`). It speaks the CVS wire protocol over stdin/stdout
//! and maintains a per-head SQLite revision database so that CVS clients see
//! stable `1.N` revision numbers across git history. It has no C
//! implementation and no plumbing equivalent.
//!
//! What is ported here (byte-identical to stock, verified against git 2.55.0):
//!   * The `Getopt::Long` front end at `git-cvsserver.perl:123-135` — the exact
//!     usage text, unique-prefix and case-insensitive long-option matching,
//!     `--version`/`-V` output, and the three `warn` + `die $usage` failure
//!     paths (`Unknown option: X`, `Option X requires an argument`,
//!     `Option X does not take an argument`), all on stderr with exit 255.
//!   * The operand handling at `:140-156`: a leading `pserver` or `server`
//!     selects the method and is dropped, everything else is an allowed root,
//!     and `--export-all` without one dies
//!     `--export-all can only be used together with an explicit '<directory>...'
//!     list`.
//!   * The `pserver` header check at `:179-183`: the first line must be
//!     `BEGIN AUTH REQUEST` or `BEGIN VERIFICATION REQUEST`, and anything else —
//!     including nothing at all, where perl's undef interpolates as the empty
//!     string — is `E Do not understand <line> - expecting BEGIN AUTH REQUEST`
//!     with exit 255.
//!   * The `while (<STDIN>)` request loop's own termination, at `:254` and
//!     `:277-278`: a client that sends nothing is not an error. The loop body
//!     never runs, `git-cvsserver` prints nothing and exits **0** — which is
//!     what `git cvsserver` with stdin closed does, and (because `-h` sets
//!     `$state->{h}` while the guard at `:132` reads `$state->{help}`, so the
//!     help branch is dead code in stock git) also what `git cvsserver -h`
//!     does.
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
//! `-h`/`-H` are *not* help in stock git: `@opts` declares `'h|H'`, which
//! populates `$state->{h}`, but the guard at `git-cvsserver.perl:132` tests
//! `$state->{help}` — so `-h` prints nothing and falls straight into the server
//! loop, where a closed stdin ends it with exit 0. That is reproduced; the usage
//! text is only ever printed by the option-parsing failure path, as in stock.

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
    let mut export_all = false;
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
                    "export-all" => export_all = true,
                    // `h`/`H` sets `$state->{h}`, which nothing reads, and
                    // `base-path`/`strict-paths` only steer request handling.
                    _ => {}
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

    // `:141-148` — a leading `pserver`/`server` names the transport and is
    // dropped; whatever remains is the allowed-roots list.
    let method_is_pserver = match operands.first() {
        Some(&"pserver") => {
            operands.remove(0);
            true
        }
        Some(&"server") => {
            operands.remove(0);
            false
        }
        _ => false,
    };

    // `:154-156`. The message ends in its own newline, so perl's `die` adds no
    // ` at <script> line <n>.` suffix.
    if export_all && operands.is_empty() {
        eprintln!("--export-all can only be used together with an explicit '<directory>...' list");
        return Ok(ExitCode::from(255));
    }

    if method_is_pserver {
        // `:180-183` — the authentication cat starts with one line that must be
        // the request header. A client that sends nothing leaves `$line` undef,
        // which `chomp`/`m//` warn about and interpolate as the empty string.
        let first = read_line()?.unwrap_or_default();
        let first = first.as_str();
        if first != "BEGIN AUTH REQUEST" && first != "BEGIN VERIFICATION REQUEST" {
            // The `die` string ends in its own newline, so perl adds no
            // ` at <script> line <n>.` suffix. Exit 255.
            eprintln!("E Do not understand {first} - expecting BEGIN AUTH REQUEST");
            return Ok(ExitCode::from(255));
        }
        bail!(
            "the pserver authentication exchange is not ported. Past the header it reads the \
             root, user and scrambled password and answers I LOVE YOU / I HATE YOU against the \
             [gitcvs] authdb, which needs the CVS root resolution the server half provides plus \
             a crypt(3) check of the descrambled password"
        );
    }

    request_loop()
}

/// `git-cvsserver.perl:254-278`'s `while (<STDIN>)`.
///
/// The end of that loop is the whole of what can be ported: a client that
/// closes the connection without sending a request leaves the loop body unrun,
/// and the script falls through to `chdir '/'; exit 0` having printed nothing.
/// Every line that *is* read dispatches into the CVS protocol handlers, which
/// are not ported — see the module docs for the missing substrate — so a real
/// request reports that rather than answering it.
fn request_loop() -> Result<ExitCode> {
    loop {
        // `while (<STDIN>)`: end of input ends the loop, and nothing else does.
        let Some(request) = read_line()? else {
            return Ok(ExitCode::SUCCESS);
        };
        let request = request.as_str();
        // `if (/^([\w-]+)(?:\s+(.*))?$/ and defined($methods->{$1}))`, else
        // `die("Unknown command $_")` — 255, since `$!` and `$?` are clear.
        let verb = match request.find(char::is_whitespace) {
            Some(at) => &request[..at],
            None => request,
        };
        let known = !verb.is_empty()
            && verb.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-')
            && METHODS.contains(&verb);
        if !known {
            eprintln!("Unknown command {request}");
            return Ok(ExitCode::from(255));
        }
        bail!(
            "the CVS request {verb:?} is not ported. Stock \
             git-cvsserver answers it from a DBD::SQLite revision database it maintains \
             per head (gitcvs.<module>.sqlite), whose schema and incremental-update rules \
             are defined only by that Perl script; the vendored gitoxide has neither a CVS \
             wire-protocol layer nor that database, and an approximation would hand a CVS \
             client wrong revision numbers and silently corrupt its sandbox"
        );
    }
}

/// One `<STDIN>` read plus `chomp`: `None` at end of input.
///
/// Read as bytes rather than as text because the CVS protocol carries file
/// names, and a client that sends one in a non-UTF-8 encoding must get the
/// protocol's own answer rather than a decoding error from this process.
fn read_line() -> Result<Option<String>> {
    let mut buf: Vec<u8> = Vec::new();
    if std::io::BufRead::read_until(&mut std::io::stdin().lock(), b'\n', &mut buf)? == 0 {
        return Ok(None);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
    }
    Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
}

/// The keys of `$methods` at `git-cvsserver.perl:58-88`, which is what decides
/// whether a request line is dispatched or is an `Unknown command` death.
const METHODS: &[&str] = &[
    "Root",
    "Valid-responses",
    "valid-requests",
    "Directory",
    "Sticky",
    "Entry",
    "Modified",
    "Unchanged",
    "Questionable",
    "Argument",
    "Argumentx",
    "expand-modules",
    "add",
    "remove",
    "co",
    "update",
    "ci",
    "diff",
    "log",
    "rlog",
    "tag",
    "status",
    "admin",
    "history",
    "watchers",
    "editors",
    "noop",
    "annotate",
    "Global_option",
];
