//! `git imap-send` — upload an mbox from stdin to an IMAP folder.
//! **The upload itself is not ported: every path that would open a connection
//! bails.**
//!
//! Stock `git-imap-send` is a C builtin (`imap-send.c`), but the work it does is
//! not git work: it is an IMAP4rev1 client. It opens a TCP or TLS socket (or
//! forks `imap.tunnel`), performs `CAPABILITY`, then `LOGIN` / `AUTHENTICATE`
//! with PLAIN, CRAM-MD5, OAUTHBEARER or XOAUTH2, then `APPEND`s each message —
//! or, under `--list`, issues `LIST "" "*"` and prints the untagged responses
//! verbatim. Optionally it delegates the same protocol to libcurl. None of that
//! has a substrate in the vendored gitoxide crates under `src/ported/`: there is
//! no IMAP client, no SASL implementation, and gitoxide's transport layer speaks
//! only the git wire protocols over ssh/http. Reimplementing an IMAP client is
//! the prerequisite, and it is out of scope for a gitoxide-backed port.
//!
//! What *is* ported is the surface that is byte-verifiable without a server:
//! the whole command line, the two configuration pre-flight checks, and the two
//! stdin rejections that happen before the socket is created. All output below
//! was captured from git 2.55.0 on Darwin.
//!
//! ### Covered (byte-identical stdout/stderr and exit code)
//!
//! * `parse_options` over the builtin's five options — `-v`/`--verbose`,
//!   `-q`/`--quiet`, `--curl`, `-f`/`--folder <folder>`, `--list` — including
//!   `--no-` forms, `--opt=value`, `-fVALUE` and `-f VALUE`, short bundling
//!   (`-vq`), unique-prefix abbreviation (`--fol`, `--l`), and `--` as a
//!   terminator.
//! * `-h`: the 408-byte usage block on **stdout**, exit 129, emitted at the
//!   point `-h` is reached (so `-h --bogus` prints usage, `--bogus -h` does
//!   not).
//! * The five `parse_options` diagnostics, all on stderr, all exit 129:
//!   ``error: unknown option `bogus'`` and ``error: unknown switch `Z'``, both
//!   followed by the usage block; ``error: option `folder' requires a value``,
//!   ``error: switch `f' requires a value`` and ``error: option `curl' takes no
//!   value``, none of which print usage. Any positional argument prints the
//!   usage block alone, exit 129.
//! * `imap.host`/`imap.tunnel` missing → `error: no IMAP host specified` plus
//!   its two `hint:` lines, exit 1. Then, unless `--list` was given,
//!   `imap.folder` missing → `error: no IMAP folder specified` plus its two
//!   `hint:` lines, exit 1. Presence is what is tested, not emptiness:
//!   `-c imap.host=` satisfies the check. Config is read whether or not the
//!   current directory is a repository.
//! * `--folder` overrides `imap.folder` only when it carries a value;
//!   `--no-folder` leaves a configured folder in place (the builtin copies the
//!   parsed string over the config value only `if (folder)`).
//! * Empty stdin → `nothing to send` on stderr, exit 1. Non-empty stdin whose
//!   `count_messages` scan finds no message → `no messages found to send` on
//!   stderr, exit 1. The scan is reproduced from observed behaviour: a message
//!   is a literal `From ` at the very start of the buffer, or after a `\nFrom `
//!   found from five bytes past the previous scan position, followed in order by
//!   `\nFrom: `, `\nDate: ` and `\nSubject: `.
//!
//! ### Not covered
//!
//! * Everything after those checks: connecting, authenticating, `APPEND`, and
//!   `--list`. These bail, naming the missing IMAP substrate. Stock git reaches
//!   the network here, so its exit status on those paths is a property of the
//!   server and credentials, not of the command line.
//! * `--no-curl`. On a git built with `USE_CURL_FOR_IMAP_SEND` and `NO_OPENSSL`
//!   it prints `warning: --no-curl not supported in this build` and continues;
//!   on other builds it silently selects the in-tree IMAP code. That diagnostic
//!   is a compile-time property of the stock binary with no analogue here, so
//!   the flag bails rather than guessing which build is being compared against.
//!   `--curl` is accepted and discarded, which is what stock git does with it on
//!   every path this module reaches.
//! * Ambiguous-abbreviation errors. No two option names in this builtin share a
//!   first letter, so `parse_options` can never report one for `imap-send`.

use anyhow::{bail, Result};
use std::io::Read;
use std::process::ExitCode;

use gix::config::File as ConfigFile;

/// The usage block from `imap_send_usage[]` plus the option table
/// `parse_options` renders under it. 408 bytes.
const USAGE: &str = concat!(
    "usage: git imap-send [-v] [-q] [--[no-]curl] [(--folder|-f) <folder>] < <mbox>\n",
    "   or: git imap-send --list\n",
    "\n",
    "    -v, --[no-]verbose    be more verbose\n",
    "    -q, --[no-]quiet      be more quiet\n",
    "    --[no-]curl           use libcurl to communicate with the IMAP server\n",
    "    -f, --[no-]folder <folder>\n",
    "                          specify the IMAP folder\n",
    "    --[no-]list           list all folders on the IMAP server\n",
    "\n",
);

/// The builtin's `imap_send_options[]`, in declaration order — the order
/// `parse_options` uses when matching an abbreviation.
const LONGS: &[(&str, bool)] = &[
    ("verbose", false),
    ("quiet", false),
    ("curl", false),
    ("folder", true),
    ("list", false),
];

/// State accumulated by the option scan.
#[derive(Default)]
struct Opts {
    /// `-f`/`--folder`: `None` unless the flag carried a value. `--no-folder`
    /// leaves this `None`, which is why it cannot clear `imap.folder`.
    folder: Option<String>,
    list: bool,
    /// `--no-curl`, which is build-conditional in stock git; see module docs.
    no_curl: bool,
    /// Any non-option argument. The builtin accepts none.
    positional: bool,
}

/// How the scan ended.
enum Scan {
    Ok(Opts),
    /// `-h`: usage on stdout, exit 129.
    Help,
    /// A diagnostic line, and whether the usage block follows it on stderr.
    Error(String, bool),
}

/// `usage_with_options()` — the block on stderr, exit 129.
fn usage_err() -> ExitCode {
    eprint!("{USAGE}");
    ExitCode::from(129)
}

/// Resolve a long option name: exact match first, then unique prefix. Returns
/// the canonical name and whether it takes a value.
fn resolve_long(name: &str) -> Option<(&'static str, bool)> {
    if name.is_empty() {
        return None;
    }
    if let Some(&(n, v)) = LONGS.iter().find(|(n, _)| *n == name) {
        return Some((n, v));
    }
    let mut hits = LONGS.iter().filter(|(n, _)| n.starts_with(name));
    let first = *hits.next()?;
    // No two names here share a first letter, so a second hit is impossible;
    // treating one as no-match would be wrong, so require uniqueness anyway.
    match hits.next() {
        None => Some(first),
        Some(_) => None,
    }
}

/// Reproduce `parse_options(..., 0)` over `imap_send_options[]`.
fn scan(args: &[String]) -> Scan {
    let mut opts = Opts::default();
    let mut it = args.iter().peekable();

    while let Some(arg) = it.next() {
        if arg == "--" {
            opts.positional |= it.next().is_some();
            break;
        }

        if let Some(body) = arg.strip_prefix("--") {
            let (spelled, value) = match body.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (body, None),
            };
            let (base, negated) = match spelled.strip_prefix("no-") {
                Some(b) => (b, true),
                None => (spelled, false),
            };
            let Some((name, takes_value)) = resolve_long(base) else {
                return Scan::Error(format!("error: unknown option `{spelled}'"), true);
            };
            // A `--no-` form never consumes a value, so `--no-folder=x` is
            // reported against the name as the user spelled it.
            if negated || !takes_value {
                if value.is_some() {
                    let shown = if negated { format!("no-{name}") } else { name.to_string() };
                    return Scan::Error(format!("error: option `{shown}' takes no value"), false);
                }
                match (name, negated) {
                    ("list", n) => opts.list = !n,
                    ("curl", true) => opts.no_curl = true,
                    ("folder", true) => {}
                    // `--verbose`, `--quiet` and their negations only steer
                    // protocol chatter, which never happens here.
                    _ => {}
                }
                continue;
            }
            let value = match value.or_else(|| it.next().cloned()) {
                Some(v) => v,
                None => {
                    return Scan::Error(format!("error: option `{name}' requires a value"), false)
                }
            };
            opts.folder = Some(value);
            continue;
        }

        // A bare `-` is an ordinary argument, as is anything not starting `-`.
        let bundle = match arg.strip_prefix('-') {
            Some(b) if !b.is_empty() => b,
            _ => {
                opts.positional = true;
                continue;
            }
        };
        let mut rest = bundle;
        while let Some(c) = rest.chars().next() {
            rest = &rest[c.len_utf8()..];
            match c {
                'h' => return Scan::Help,
                'v' | 'q' => {}
                'f' => {
                    // The remainder of the bundle is the value, else the next
                    // argument.
                    let value = if rest.is_empty() {
                        it.next().cloned()
                    } else {
                        Some(std::mem::take(&mut rest).to_string())
                    };
                    match value {
                        Some(v) => opts.folder = Some(v),
                        None => {
                            return Scan::Error("error: switch `f' requires a value".into(), false)
                        }
                    }
                    break;
                }
                _ => return Scan::Error(format!("error: unknown switch `{c}'"), true),
            }
        }
    }

    Scan::Ok(opts)
}

/// The config git would see: the repository in the current directory when there
/// is one, otherwise the global and system files alone. `imap-send` runs
/// `setup_git_directory_gently()`, so it works outside a repository.
fn load_config() -> Option<ConfigFile> {
    match gix::discover(".") {
        Ok(repo) => Some(repo.config_snapshot().plumbing().clone()),
        Err(_) => {
            let mut file = ConfigFile::from_globals().ok()?;
            file.append(ConfigFile::from_environment_overrides().ok()?).ok()?;
            Some(file)
        }
    }
}

/// `count_messages()` — how many `From `-delimited messages carrying `From:`,
/// `Date:` and `Subject:` headers, in that order, the buffer holds.
///
/// Only the zero / non-zero distinction is observable without a server, but the
/// scan is reproduced faithfully: positions advance the way the C `strstr`
/// chain does, so a header block that appears before its `From ` line, or a
/// `From ` line that is not at the start of the buffer and not reachable from
/// five bytes past the previous match, does not count.
fn count_messages(buf: &[u8]) -> usize {
    // The C code scans a NUL-terminated `strbuf`, so an embedded NUL ends it.
    let buf = match buf.iter().position(|&b| b == 0) {
        Some(n) => &buf[..n],
        None => buf,
    };
    let find = |from: usize, needle: &[u8]| -> Option<usize> {
        if from > buf.len() {
            return None;
        }
        buf[from..]
            .windows(needle.len())
            .position(|w| w == needle)
            .map(|i| i + from)
    };

    let mut count = 0;
    let mut p = 0usize;
    loop {
        if buf[p..].starts_with(b"From ") {
            let Some(i) = find(p + 5, b"\nFrom: ") else { break };
            let Some(j) = find(i + 7, b"\nDate: ") else { break };
            let Some(k) = find(j + 7, b"\nSubject: ") else { break };
            p = k + 10;
            count += 1;
        }
        let Some(n) = find(p + 5, b"\nFrom ") else { break };
        p = n + 1;
    }
    count
}

/// `git imap-send` — send a collection of patches from stdin to an IMAP folder.
///
/// Parses the command line as the builtin's `parse_options` call does and
/// reproduces every path that terminates before a connection is attempted:
/// `-h`, the option diagnostics, the two configuration checks, and the two
/// stdin rejections. Anything that would talk IMAP bails, naming the missing
/// substrate.
pub fn imap_send(args: &[String]) -> Result<ExitCode> {
    let opts = match scan(args) {
        Scan::Help => {
            print!("{USAGE}");
            return Ok(ExitCode::from(129));
        }
        Scan::Error(msg, with_usage) => {
            eprintln!("{msg}");
            if with_usage {
                eprint!("{USAGE}");
            }
            return Ok(ExitCode::from(129));
        }
        Scan::Ok(opts) => opts,
    };

    // `if (argc) usage_with_options(...)`, checked after the whole scan.
    if opts.positional {
        return Ok(usage_err());
    }

    if opts.no_curl {
        bail!(
            "unsupported flag \"--no-curl\": whether stock git warns or silently switches IMAP \
             backends depends on how it was compiled (USE_CURL_FOR_IMAP_SEND / NO_OPENSSL), and \
             neither backend exists here (ported: --curl, -v, -q, -f/--folder, --list, -h)"
        );
    }

    let cfg = load_config();
    let get = |key: &str| cfg.as_ref().and_then(|c| c.string(key)).map(|v| v.to_string());

    // Presence, not emptiness: `-c imap.host=` satisfies this.
    if get("imap.host").is_none() && get("imap.tunnel").is_none() {
        eprintln!("error: no IMAP host specified");
        eprintln!("hint: set the IMAP host with 'git config imap.host <host>'.");
        eprintln!("hint: (e.g., 'git config imap.host imaps://imap.example.com')");
        return Ok(ExitCode::from(1));
    }

    // `--folder` overwrites the config value only when it carried one.
    let folder = opts.folder.or_else(|| get("imap.folder"));

    if !opts.list && folder.is_none() {
        eprintln!("error: no IMAP folder specified");
        eprintln!("hint: set the target folder with 'git config imap.folder <folder>'.");
        eprintln!("hint: (e.g., 'git config imap.folder Drafts')");
        return Ok(ExitCode::from(1));
    }

    if opts.list {
        bail!(
            "unsupported: --list needs an IMAP client (connect, authenticate, LIST \"\" \"*\"), \
             which no vendored crate under src/ported provides (ported: option parsing, -h, the \
             imap.host/imap.tunnel and imap.folder checks)"
        );
    }

    let mut mbox = Vec::new();
    std::io::stdin().lock().read_to_end(&mut mbox)?;

    if mbox.is_empty() {
        eprintln!("nothing to send");
        return Ok(ExitCode::from(1));
    }
    let total = count_messages(&mbox);
    if total == 0 {
        eprintln!("no messages found to send");
        return Ok(ExitCode::from(1));
    }

    bail!(
        "unsupported: sending {total} message(s) needs an IMAP client (TLS or imap.tunnel, \
         LOGIN/CRAM-MD5/OAUTHBEARER/XOAUTH2, APPEND), which no vendored crate under src/ported \
         provides (ported: option parsing, -h, the config checks, and the mbox message scan)"
    );
}
