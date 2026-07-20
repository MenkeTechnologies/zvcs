//! `git remote-ftp` — the FTP/FTPS remote helper.
//!
//! On a stock install `git-remote-ftp` (and `git-remote-ftps`) is a symlink to
//! `git-remote-http`, i.e. `remote-curl.c`. For an `ftp://` URL that binary has
//! exactly one mode: the **dumb** walker (`http-walker.c` driving libcurl), which
//! GETs `info/refs`, `objects/info/packs`, loose objects and packfiles over FTP.
//! There is no smart protocol over FTP and no push.
//!
//! ### What is covered here
//!
//! The parts of `remote-curl.c` that are transport-independent, verified
//! byte-for-byte against stock git (stdout, stderr and exit code):
//!
//! * `git remote-ftp` with no arguments —
//!   `error: remote-curl: usage: git remote-curl <remote> [<url>]`, exit 1.
//! * the startup URL check: a URL with no `://` produces
//!   `warning: url has no scheme: <url>/` followed by
//!   `fatal: credential url cannot be parsed: <url>/`, exit 128 (the trailing
//!   slash is `remote-curl`'s `str_end_url_with_slash`; an empty URL gets none).
//! * the remote-helper command loop on stdin: `capabilities` prints the helper's
//!   capability list, `option <name> [<value>]` answers `ok` /`unsupported` /
//!   `error invalid value` per `remote-curl.c::set_option`, `option object-format
//!   <bad>` dies with `fatal: unknown value for object-format: <bad>` (exit 128),
//!   an empty line ends the loop with exit 0, EOF ends it with exit 1, and any
//!   other line is `error: remote-curl: unknown command '<line>' from git`
//!   (exit 1). Command matching is exact — `capabilities ` with a trailing space
//!   is an unknown command, as it is for git.
//!
//! ### What is NOT covered — and why it is not faked
//!
//! `list`, `list for-push`, `fetch`, `get`, `push`, `stateless-connect` and
//! `check-connectivity` all bail. Every one of them needs substrate that the
//! vendored gitoxide crates do not contain:
//!
//! * **No FTP client.** `gix-transport`'s only network backends are `git://`,
//!   `ssh` and HTTP (curl/reqwest); `gix-url` does not even have an FTP scheme —
//!   `ftp` falls into `Scheme::Ext` (`gix-url/src/scheme.rs:32`).
//! * **No dumb protocol.** gitoxide implements the smart protocol only and says
//!   so explicitly: "`dumb` protocol is not supported"
//!   (`gix-transport/src/client/blocking_io/http/mod.rs:299`). The object walker
//!   that `remote-curl` uses for FTP — `info/refs` parsing, alternates chasing,
//!   `objects/info/packs`, loose-object and pack download — has no counterpart.
//!
//! Implementing those would mean writing an FTP client and a dumb-transport
//! walker from scratch, not porting onto existing substrate, so they report the
//! missing piece instead of returning plausible-looking output.
//!
//! The `<remote> <url>` form with the URL omitted also bails: git falls back to
//! the configured remote's URL through `remote_get`, whose fallback semantics for
//! unconfigured names are not reproduced here.

use anyhow::{bail, Result};
use std::io::{Read, Write};
use std::process::ExitCode;

/// `remote-curl.c`'s usage string, byte-for-byte (it names `remote-curl`, not
/// `remote-ftp`, because the FTP helper is the same binary).
const USAGE: &str = "error: remote-curl: usage: git remote-curl <remote> [<url>]\n";

/// The capability list `remote-curl` advertises, in its order, terminated by the
/// blank line that ends a helper response.
const CAPABILITIES: &str = "stateless-connect\nfetch\nget\noption\npush\ncheck-connectivity\nobject-format\n\n";

/// `git remote-ftp` — see the module docs for the covered behaviour.
pub fn remote_ftp(args: &[String]) -> Result<ExitCode> {
    // argv[0] is the command, argv[1] the remote name, argv[2] the URL. Only
    // "no remote at all" is a usage error; extra arguments are ignored.
    if args.len() < 2 {
        eprint!("{USAGE}");
        return Ok(ExitCode::from(1));
    }
    let Some(url) = args.get(2) else {
        bail!("remote-ftp <remote> without <url> is unsupported (the URL must be given explicitly)");
    };

    // `str_end_url_with_slash` then `credential_from_url`, which only fails at
    // this point when the URL carries no scheme at all.
    let with_slash = if url.is_empty() || url.ends_with('/') {
        url.clone()
    } else {
        format!("{url}/")
    };
    if !with_slash.contains("://") {
        eprintln!("warning: url has no scheme: {with_slash}");
        eprintln!("fatal: credential url cannot be parsed: {with_slash}");
        return Ok(ExitCode::from(128));
    }

    command_loop()
}

/// `remote-curl.c::main`'s command loop over stdin.
///
/// Returns exit 0 on a blank line (git's clean end-of-batch) and exit 1 on EOF or
/// an unknown command, matching the observed behaviour of the stock helper.
fn command_loop() -> Result<ExitCode> {
    loop {
        let Some(line) = read_line()? else {
            // EOF without a terminating blank line.
            return Ok(ExitCode::from(1));
        };
        if line.is_empty() {
            return Ok(ExitCode::SUCCESS);
        }

        if line == "capabilities" {
            let mut out = std::io::stdout();
            out.write_all(CAPABILITIES.as_bytes())?;
            out.flush()?;
            continue;
        }

        if let Some(rest) = line.strip_prefix("option ") {
            match set_option(rest) {
                OptionReply::Answer(reply) => {
                    let mut out = std::io::stdout();
                    writeln!(out, "{reply}")?;
                    out.flush()?;
                }
                OptionReply::Die(msg) => {
                    eprintln!("fatal: {msg}");
                    return Ok(ExitCode::from(128));
                }
            }
            continue;
        }

        // The transport commands. Each needs the dumb FTP walker; see module docs.
        if line == "list"
            || line == "list for-push"
            || line.starts_with("fetch ")
            || line.starts_with("get ")
            || line.starts_with("push ")
            || line.starts_with("stateless-connect ")
            || line == "check-connectivity"
        {
            let verb = line.split(' ').next().unwrap_or(&line).to_string();
            bail!(
                "remote-ftp {verb:?} is unsupported: no FTP transport and no dumb-protocol \
                 walker in the vendored gitoxide crates (ported: capabilities, option)"
            );
        }

        eprintln!("error: remote-curl: unknown command '{line}' from git");
        return Ok(ExitCode::from(1));
    }
}

/// The two shapes a `set_option` result can take: a line sent back to git, or a
/// `die()` that ends the process.
enum OptionReply {
    Answer(&'static str),
    Die(String),
}

/// `remote-curl.c::set_option` — `rest` is everything after `option `.
///
/// The option name runs to the first space; the value is the entire remainder
/// (so `option verbosity  2` has the value `" 2"`, which `strtol` accepts), and
/// is empty when there is no space at all.
fn set_option(rest: &str) -> OptionReply {
    let (name, value) = match rest.split_once(' ') {
        Some((n, v)) => (n, v),
        None => (rest, ""),
    };

    let ok_if = |cond: bool| {
        if cond {
            OptionReply::Answer("ok")
        } else {
            OptionReply::Answer("error invalid value")
        }
    };

    match name {
        // Integers, parsed with strtol: the whole value must be consumed.
        "verbosity" | "depth" => ok_if(parse_c_long(value)),

        // Booleans, compared literally against "true"/"false" — unlike config
        // parsing, `yes`, `on`, `1` and `0` are all rejected here.
        "progress" | "dry-run" | "followtags" | "check-connectivity" | "cloning"
        | "update-shallow" | "from-promisor" | "atomic" | "force-if-includes"
        | "deepen-relative" | "refetch" => ok_if(value == "true" || value == "false"),

        // Values passed straight through to the transport, never validated here.
        "deepen-since" | "deepen-not" | "cas" | "push-option" | "filter" => {
            OptionReply::Answer("ok")
        }

        "pushcert" => ok_if(value == "true" || value == "false" || value == "if-asked"),
        "family" => ok_if(value == "ipv4" || value == "ipv6" || value == "all"),

        // The only accepted forms are an empty value and "true"; anything else
        // is fatal rather than an error line.
        "object-format" => {
            if value.is_empty() || value == "true" {
                OptionReply::Answer("ok")
            } else {
                OptionReply::Die(format!("unknown value for object-format: {value}"))
            }
        }

        // Options remote-curl knows nothing about, including ones other helpers
        // implement (`servpath`, `no-dependents`, `sideband-all`).
        _ => OptionReply::Answer("unsupported"),
    }
}

/// Whether `strtol(value, &end, 10)` would consume all of `value`.
///
/// Leading whitespace and a sign are accepted, at least one digit is required,
/// and nothing may follow the digits — `git`'s check is `*end` being NUL.
fn parse_c_long(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
        i += 1;
    }
    if matches!(bytes.get(i), Some(b'+') | Some(b'-')) {
        i += 1;
    }
    let first_digit = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    i > first_digit && i == bytes.len()
}

/// One `strbuf_getline_lf` worth of stdin: the line without its `\n`, or `None`
/// at EOF.
///
/// Read unbuffered from descriptor 0 so that payload bytes belonging to a later
/// command are never swallowed by a buffered reader.
fn read_line() -> Result<Option<String>> {
    let mut stdin = std::io::stdin();
    let mut line = Vec::with_capacity(128);
    let mut byte = [0u8; 1];

    loop {
        match stdin.read(&mut byte) {
            Ok(0) => break,
            Ok(_) if byte[0] == b'\n' => return Ok(Some(String::from_utf8_lossy(&line).into_owned())),
            Ok(_) => line.push(byte[0]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => bail!("error reading command stream from git"),
        }
    }

    if line.is_empty() {
        Ok(None)
    } else {
        // A final line without a newline is still delivered by strbuf_getline_lf.
        Ok(Some(String::from_utf8_lossy(&line).into_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_c_long, set_option, OptionReply};

    fn reply(rest: &str) -> String {
        match set_option(rest) {
            OptionReply::Answer(a) => a.to_string(),
            OptionReply::Die(m) => format!("fatal: {m}"),
        }
    }

    #[test]
    fn option_replies_match_stock_remote_curl() {
        // Verified against `git remote-ftp origin ftp://h/p`.
        assert_eq!(reply("zzz 1"), "unsupported");
        assert_eq!(reply("servpath /x"), "unsupported");
        assert_eq!(reply("no-dependents true"), "unsupported");
        assert_eq!(reply("verbosity 3"), "ok");
        assert_eq!(reply("verbosity"), "error invalid value");
        assert_eq!(reply("verbosity  2"), "ok");
        assert_eq!(reply("depth 1 2"), "error invalid value");
        assert_eq!(reply("depth -1"), "ok");
        // Only the literal words are booleans here.
        assert_eq!(reply("progress true"), "ok");
        assert_eq!(reply("progress yes"), "error invalid value");
        assert_eq!(reply("progress 0"), "error invalid value");
        assert_eq!(reply("atomic Off"), "error invalid value");
        assert_eq!(reply("pushcert if-asked"), "ok");
        assert_eq!(reply("pushcert x"), "error invalid value");
        assert_eq!(reply("family ipv4"), "ok");
        assert_eq!(reply("family bogus"), "error invalid value");
        assert_eq!(reply("filter"), "ok");
        assert_eq!(reply("object-format true"), "ok");
        assert_eq!(reply("object-format"), "ok");
        assert_eq!(reply("object-format false"), "fatal: unknown value for object-format: false");
    }

    #[test]
    fn c_long_consumes_the_whole_value() {
        assert!(parse_c_long("007"));
        assert!(parse_c_long(" 2"));
        assert!(parse_c_long("-1"));
        assert!(!parse_c_long(""));
        assert!(!parse_c_long("3x"));
        assert!(!parse_c_long("1 2"));
    }
}
