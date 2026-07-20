//! `git instaweb` — browse the working repository in gitweb.
//! **The web server is not ported: every path that would serve gitweb bails.**
//!
//! Stock `git-instaweb` is a 786-line POSIX shell script
//! (`$(git --exec-path)/git-instaweb`, `#!/bin/sh` on line 1). It contains no
//! git object logic at all. What it does is generate a config file for one of
//! six external HTTP daemons — `lighttpd`, `apache2`/`httpd`, `mongoose`,
//! `plackup`, `webrick`, `python` (`configure_httpd`, line 728) — write a
//! `gitweb_config.perl` (line 717), exec the daemon, poll `127.0.0.1:$port`
//! from an inline `perl -MIO::Socket::INET` one-liner (line 151), and hand the
//! URL to `git web--browse`. The page it serves is `gitweb.cgi`, a Perl CGI
//! program shipped separately under git's `share/gitweb`.
//!
//! None of that has a substrate in the vendored gitoxide crates under
//! `src/ported`: there is no HTTP server, no CGI host, no Perl interpreter, and
//! no gitweb. The serving half is therefore refused rather than approximated —
//! a fabricated "server" would diverge on the only thing instaweb produces.
//!
//! ### Covered (byte-identical streams and exit codes against git 2.55.0)
//!
//! Everything the script does before it needs a daemon, all of which is
//! argument, repository-discovery and file handling:
//!
//! * The `git rev-parse --parseopt` front end that `git-sh-setup` (line 71)
//!   drives from the script's `OPTIONS_SPEC` (lines 9-21). `OPTIONS_KEEPDASHDASH`
//!   and `OPTIONS_STUCKLONG` are both empty, so neither `--keep-dashdash` nor
//!   `--stuck-long` applies. Reproduced: short bundling (`-lp 1234`), attached
//!   short values (`-p1234`), `--long=value`, detached long values, `--no-`
//!   negation of every option, unambiguous long-name abbreviation, and `--` as
//!   the option terminator. Options are permuted ahead of positionals exactly
//!   as parseopt emits them in its `set -- …` line.
//! * parseopt's five diagnostics, each with its own stream split, all exit 129:
//!   - `-h` → the 530-byte usage block on **stdout** (parseopt writes a
//!     `cat <<\EOF … EOF` snippet the script evals).
//!   - ``error: unknown option `x'`` / ``error: unknown switch `x'`` → the error
//!     **and** the usage block on stderr.
//!   - ``error: option `port' requires a value`` / ``error: switch `p' requires
//!     a value`` / ``error: option `local' takes no value`` → the error alone on
//!     stderr, no usage block.
//!   - `error: ambiguous option: s (could be --stop or --start)` → the error on
//!     stderr, the usage block on **stdout**.
//! * The script's own `*)` fallthrough (line 196) for any token parseopt passes
//!   through that the `case` does not name — a stray positional, or a `--no-…`
//!   form, since the case matches neither. `git-sh-setup`'s `usage()` re-execs
//!   `"$0" -h`, so this prints the usage block on **stdout** and exits **1**,
//!   not 129.
//! * `git_dir_init` (`git-sh-setup` line 326), which runs *after* parseopt: no
//!   repository → `fatal: not a git repository (or any of the parent
//!   directories): .git` on stderr, exit 128. `SUBDIRECTORY_OK=Yes`, so a
//!   subdirectory is fine, and `GIT_DIR` is made absolute.
//! * `mkdir -p "$GIT_DIR/gitweb/tmp"` (line 203), which runs for *every* action
//!   including `--stop`, before the action `case` at line 755.
//! * `--stop`/`stop` when no `$GIT_DIR/pid` exists: `stop_httpd` (line 145) is
//!   a no-op and the script exits 0 having only created `gitweb/tmp`.
//!
//! ### Not covered — these `bail!` rather than fake a result
//!
//! * `--start`, `--restart` and the default `browse` action: they need one of
//!   the six external HTTP daemons plus gitweb.cgi; see above.
//! * `--stop` with a live `$GIT_DIR/pid`. The script runs
//!   `kill $(cat "$fqgitdir/pid")` — unquoted, so every whitespace-separated
//!   word becomes an argument — then `rm -f` the file. This crate's dependency
//!   set is `gix` and `anyhow` only (`src/extensions/Cargo.toml` line 19); there
//!   is no `libc`/`nix` signal API, and shelling out to `/bin/kill` would emit
//!   that binary's diagnostics rather than the shell builtin's on a bad pid.
//!   Refused instead of half-done, since the pid file's removal is post-command
//!   state a differential harness inspects.
//! * `resolve_full_httpd`'s `"$httpd_only not found. Install …"` failure (line
//!   97). It searches `PATH`, `/usr/local/sbin`, `/usr/sbin`, then git's
//!   installed `share/gitweb` — a path belonging to the git installation, not
//!   to zvcs — so its outcome is not reproducible here.
//! * `instaweb.local`/`.httpd`/`.gitwebdir`/`.port`/`.modulepath` config
//!   (lines 27-31) is not read: every value it could set feeds only the daemon
//!   paths above, and reading it would change nothing observable.
//!
//! Known deviation: parseopt reports an ambiguous abbreviation with exactly the
//! first two matching names, which is all this spec can produce (no prefix here
//! matches three options). A three-way form is not implemented.

use anyhow::{bail, Result};
use std::process::ExitCode;

/// The usage block `git rev-parse --parseopt` renders from `OPTIONS_SPEC`.
/// 530 bytes; the option column is padded to width 22 and long entries wrap.
const USAGE: &str = concat!(
    "usage: git instaweb [options] (--start | --stop | --restart)\n",
    "\n",
    "    -l, --[no-]local      only bind on 127.0.0.1\n",
    "    -p, --[no-]port ...   the port to bind to\n",
    "    -d, --[no-]httpd ...  the command to launch\n",
    "    -b, --[no-]browser ...\n",
    "                          the browser to launch\n",
    "    -m, --[no-]module-path ...\n",
    "                          the module path (only needed for apache2)\n",
    "\n",
    "Action\n",
    "    --[no-]stop           stop the web server\n",
    "    --[no-]start          start the web server\n",
    "    --[no-]restart        restart the web server\n",
    "\n",
);

/// One entry of `OPTIONS_SPEC` (lines 12-20): the long name, the optional short
/// letter, and whether the spec spells it with a trailing `=`.
struct Spec {
    long: &'static str,
    short: Option<char>,
    takes_value: bool,
}

/// The spec in declaration order — the order parseopt scans, and therefore the
/// order in which it names candidates in an ambiguity error.
const SPECS: &[Spec] = &[
    Spec { long: "local", short: Some('l'), takes_value: false },
    Spec { long: "port", short: Some('p'), takes_value: true },
    Spec { long: "httpd", short: Some('d'), takes_value: true },
    Spec { long: "browser", short: Some('b'), takes_value: true },
    Spec { long: "module-path", short: Some('m'), takes_value: true },
    Spec { long: "stop", short: None, takes_value: false },
    Spec { long: "start", short: None, takes_value: false },
    Spec { long: "restart", short: None, takes_value: false },
];

/// Where parseopt puts the usage block for a given outcome, if anywhere.
enum Usage {
    None,
    Stdout,
    Stderr,
}

/// A parseopt exit: an optional `error:` line on stderr plus a usage block.
/// Every one of these leaves with status 129.
struct Fail {
    error: Option<String>,
    usage: Usage,
}

/// `git instaweb` — browse the working repository in gitweb.
///
/// Parses the command line exactly as `git rev-parse --parseopt` does for this
/// script's `OPTIONS_SPEC`, then reproduces every path that terminates before an
/// HTTP daemon is needed. Anything that would serve gitweb bails, naming the
/// missing substrate.
pub fn instaweb(args: &[String]) -> Result<ExitCode> {
    // The dispatcher passes the argument tail; tolerate the subcommand at
    // index 0 so both calling conventions behave identically.
    let args: &[String] = match args.first() {
        Some(a) if a == "instaweb" => &args[1..],
        _ => args,
    };

    // parseopt runs inside `git-sh-setup` before `git_dir_init`, so every
    // diagnostic below is emitted whether or not there is a repository.
    let tokens = match parseopt(args) {
        Ok(tokens) => tokens,
        Err(fail) => {
            if let Some(error) = &fail.error {
                eprintln!("{error}");
            }
            match fail.usage {
                Usage::None => {}
                Usage::Stdout => print!("{USAGE}"),
                Usage::Stderr => eprint!("{USAGE}"),
            }
            return Ok(ExitCode::from(129));
        }
    };

    // The script's `while test $# != 0` loop (lines 163-201) over the tokens
    // parseopt handed back. Only the action is observable here; the daemon,
    // browser, port and module path all feed paths that bail.
    let mut action = Action::Browse;
    let mut tokens = tokens.into_iter();
    while let Some(token) = tokens.next() {
        match token.as_str() {
            "--stop" | "stop" => action = Action::Stop,
            "--start" | "start" => action = Action::Start,
            "--restart" | "restart" => action = Action::Restart,
            "-l" | "--local" => {}
            // `shift; var="$1"` — parseopt guarantees the value is present.
            "-d" | "--httpd" | "-b" | "--browser" | "-p" | "--port" | "-m" | "--module-path" => {
                tokens.next();
            }
            "--" => {}
            // `*) usage`, i.e. `"$0" -h; exit 1`: usage on stdout, status 1.
            _ => {
                print!("{USAGE}");
                return Ok(ExitCode::from(1));
            }
        }
    }

    // `git_dir_init`: `GIT_DIR=$(git rev-parse --git-dir) || exit`, then
    // `GIT_DIR=$(cd "$GIT_DIR" && pwd)` to make it absolute.
    let Ok(repo) = gix::discover(".") else {
        eprintln!("fatal: not a git repository (or any of the parent directories): .git");
        return Ok(ExitCode::from(128));
    };
    let git_dir = repo
        .git_dir()
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("Unable to determine absolute path of git directory: {e}"))?;

    // Line 203, ahead of the action dispatch and so run for every action.
    std::fs::create_dir_all(git_dir.join("gitweb").join("tmp"))?;

    let pid_file = git_dir.join("pid");
    match action {
        // `stop_httpd` with no pid file: `test -f` fails, `rm -f` is silent,
        // `exit 0`.
        Action::Stop if !pid_file.is_file() => Ok(ExitCode::SUCCESS),
        Action::Stop => bail!(
            "unsupported: stopping the instaweb daemon needs to signal the pid in {} \
             (kill(2)), and this crate depends only on gix and anyhow — no signal API \
             (ported: option parsing, repository discovery, gitweb/tmp, stop with no pid file)",
            pid_file.display()
        ),
        Action::Start | Action::Restart | Action::Browse => bail!(
            "unsupported command \"instaweb\": serving the repository requires an external HTTP \
             daemon (lighttpd, apache2, mongoose, plackup, webrick or python) and gitweb.cgi, a \
             Perl CGI program; no vendored crate under src/ported provides an HTTP server, a CGI \
             host or a Perl interpreter"
        ),
    }
}

/// The script's `action` variable (line 32), defaulting to `browse`.
enum Action {
    Browse,
    Stop,
    Start,
    Restart,
}

/// Reproduce `git rev-parse --parseopt -- "$@"` over [`SPECS`], returning the
/// token list its `set -- …` line would install: options first in the form
/// parseopt normalises them to (short letter when the spec has one, else the
/// long name; `--no-<long>` for negations; a value option followed by its
/// value), then `--`, then the positionals in order.
fn parseopt(args: &[String]) -> Result<Vec<String>, Fail> {
    let mut out: Vec<String> = Vec::new();
    let mut positional: Vec<String> = Vec::new();
    let mut rest = args.iter();
    let mut no_more_opts = false;

    while let Some(arg) = rest.next() {
        let arg = arg.as_str();
        if no_more_opts {
            positional.push(arg.to_string());
            continue;
        }
        if arg == "--" {
            no_more_opts = true;
            continue;
        }
        if let Some(name) = arg.strip_prefix("--") {
            parse_long(name, &mut rest, &mut out)?;
            continue;
        }
        // A bare `-`, and anything not starting with `-`, is a positional.
        let bundle = match arg.strip_prefix('-') {
            Some(b) if !b.is_empty() => b,
            _ => {
                positional.push(arg.to_string());
                continue;
            }
        };
        parse_shorts(bundle, &mut rest, &mut out)?;
    }

    out.push("--".to_string());
    out.extend(positional);
    Ok(out)
}

/// One `--name`, `--name=value` or `--no-name` argument.
fn parse_long<'a>(
    name: &str,
    rest: &mut impl Iterator<Item = &'a String>,
    out: &mut Vec<String>,
) -> Result<(), Fail> {
    // parseopt's own `--help`, which behaves as `-h` does.
    if name == "help" {
        return Err(Fail { error: None, usage: Usage::Stdout });
    }
    let (name, attached) = match name.split_once('=') {
        Some((n, v)) => (n, Some(v.to_string())),
        None => (name, None),
    };

    // Candidates are the long names plus their `no-` forms; an exact match wins
    // outright, otherwise a unique prefix matches and two matches are ambiguous.
    let candidates: Vec<(String, &Spec)> = SPECS
        .iter()
        .flat_map(|spec| {
            [
                (spec.long.to_string(), spec),
                (format!("no-{}", spec.long), spec),
            ]
        })
        .collect();
    let matched = match candidates.iter().find(|(full, _)| full == name) {
        Some(hit) => hit,
        None => {
            let mut hits = candidates.iter().filter(|(full, _)| full.starts_with(name));
            match (hits.next(), hits.next()) {
                (Some(hit), None) => hit,
                (Some((a, _)), Some((b, _))) => {
                    return Err(Fail {
                        error: Some(format!(
                            "error: ambiguous option: {name} (could be --{a} or --{b})"
                        )),
                        usage: Usage::Stdout,
                    })
                }
                _ => {
                    return Err(Fail {
                        error: Some(format!("error: unknown option `{name}'")),
                        usage: Usage::Stderr,
                    })
                }
            }
        }
    };
    let (full, spec) = (matched.0.as_str(), matched.1);

    // A negation never takes a value and is always emitted in long form; the
    // script's `case` names none of these, so they reach its `*)` arm.
    if let Some(long) = full.strip_prefix("no-") {
        if attached.is_some() {
            return Err(Fail {
                error: Some(format!("error: option `{full}' takes no value")),
                usage: Usage::None,
            });
        }
        out.push(format!("--no-{long}"));
        return Ok(());
    }

    let emitted = match spec.short {
        Some(c) => format!("-{c}"),
        None => format!("--{}", spec.long),
    };
    if !spec.takes_value {
        if attached.is_some() {
            return Err(Fail {
                error: Some(format!("error: option `{full}' takes no value")),
                usage: Usage::None,
            });
        }
        out.push(emitted);
        return Ok(());
    }
    let Some(value) = attached.or_else(|| rest.next().cloned()) else {
        return Err(Fail {
            error: Some(format!("error: option `{full}' requires a value")),
            usage: Usage::None,
        });
    };
    out.push(emitted);
    out.push(value);
    Ok(())
}

/// One `-abc` bundle: flags accumulate and the first value-taking letter
/// consumes the remainder of the bundle, or the next argument when empty.
fn parse_shorts<'a>(
    bundle: &str,
    rest: &mut impl Iterator<Item = &'a String>,
    out: &mut Vec<String>,
) -> Result<(), Fail> {
    let mut tail = bundle;
    while let Some(c) = tail.chars().next() {
        tail = &tail[c.len_utf8()..];
        if c == 'h' {
            return Err(Fail { error: None, usage: Usage::Stdout });
        }
        let Some(spec) = SPECS.iter().find(|s| s.short == Some(c)) else {
            return Err(Fail {
                error: Some(format!("error: unknown switch `{c}'")),
                usage: Usage::Stderr,
            });
        };
        if !spec.takes_value {
            out.push(format!("-{c}"));
            continue;
        }
        let value = if tail.is_empty() {
            rest.next().cloned()
        } else {
            Some(std::mem::take(&mut tail).to_string())
        };
        let Some(value) = value else {
            return Err(Fail {
                error: Some(format!("error: switch `{c}' requires a value")),
                usage: Usage::None,
            });
        };
        out.push(format!("-{c}"));
        out.push(value);
        break;
    }
    Ok(())
}
