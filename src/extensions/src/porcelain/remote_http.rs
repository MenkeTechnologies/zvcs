//! `git remote-http` — the smart-HTTP remote helper (stock git's `remote-curl`).
//!
//! This is not a user-facing command: git's transport layer spawns it as
//! `git remote-http <remote> <url>` and drives it over pipes with the
//! remote-helper protocol (`gitremote-helpers(7)`) — one command per line on
//! stdin, a response block terminated by a blank line on stdout.
//!
//! Ported here, byte-verified against git 2.55.0:
//!
//!   * **Argument handling** — `argc < 2` prints
//!     `error: remote-curl: usage: git remote-curl <remote> [<url>]` on stderr
//!     and exits 1; the URL is `argv[2]` falling back to `argv[1]` (the remote
//!     name), arguments past `argv[2]` are ignored; `end_url_with_slash()`
//!     appends a `/` to a non-empty URL that lacks one; the resulting URL is
//!     then run through git's `credential_from_url_gently()` scheme check,
//!     which on failure prints `warning: url has no scheme: <url>` plus
//!     `fatal: credential url cannot be parsed: <url>` and exits 128.
//!   * **The command loop** — EOF returns 1 silently, a blank line returns 0,
//!     an unrecognised line prints
//!     `error: remote-curl: unknown command '<line>' from git` and returns 1.
//!   * **`capabilities`** — the exact 7-line block plus its terminating blank
//!     line that a curl-backed git 2.55.0 advertises.
//!   * **`option <name> [<value>]`** — the complete `set_option()` table with
//!     git's exact acceptance rules: a missing value defaults to `true`;
//!     booleans are a literal `true`/`false` string compare (not
//!     `git_parse_maybe_bool`, so `TRUE`, `yes`, `on`, `1` are all rejected);
//!     `verbosity` and `depth` are `strtol` with a full-consumption check;
//!     `pushcert` additionally accepts `if-asked`; `from-promisor` accepts
//!     anything; `object-format` accepts only `true` and otherwise dies with
//!     `fatal: unknown value for object-format: <value>` (exit 128). Responses
//!     are `ok`, `unsupported` and `error invalid value`, and the loop keeps
//!     going after an `error invalid value`.
//!   * **`list`** — the ref advertisement, rendered as remote-curl's
//!     `output_refs()` does: `:object-format <algo>` first when
//!     `option object-format true` was set, then one `<oid> SP <name>` line per
//!     advertised ref (a symbolic ref instead prints `@<target> SP <name>`, and
//!     an annotated tag prints the *tag* object id, since remote-curl folds the
//!     `^{}` rows into the ref's peeled field rather than emitting them), in
//!     advertisement order, followed by the blank terminator.
//!
//! Not ported — these bail rather than fabricating a response the caller would
//! act on:
//!
//!   * **`push`** — the vendored gitoxide has no `git-receive-pack` /
//!     send-pack driver at all. `gix-protocol` implements only the fetch half;
//!     there is no API to encode the ref-update command list, generate and
//!     stream the pack, or parse `report-status`. `list for-push` is likewise
//!     out: it is a `git-receive-pack` service advertisement, and `ref_map()`
//!     only performs an `upload-pack` handshake.
//!   * **`fetch`** — the helper contract is "download the objects for these
//!     exact ids and write **no** refs". `gix`'s fetch is refspec-driven and
//!     updates refs as part of `receive()`; there is no oid-list entry point,
//!     and none of the `option depth` / `deepen-*` / `filter` state this loop
//!     records can be handed to it.
//!   * **`stateless-connect`** — requires proxying raw pkt-lines between stdin
//!     and stdout across a series of HTTP POSTs. `gix-transport`'s HTTP client
//!     exposes a typed `RequestWriter`/`ExtendedBufRead` request-response pair,
//!     not a duplex byte channel that can be bridged to the process's stdio.
//!   * **`get <url> <path>`** — a plain authenticated HTTP download to a file
//!     (used for bundle-uri). `gix-transport`'s HTTP layer is private to the
//!     git services and exposes no general GET-to-file API.
//!
//! (`check-connectivity` and `object-format` are advertised capabilities, not
//! commands — they announce that `option check-connectivity` / `option
//! object-format` are understood, and as commands git rejects them as unknown,
//! which this port does too.)
//!
//! Two deliberate divergences on the `list` path, both consequences of the
//! substrate rather than choices: `gix` resolves transport, credential and
//! `insteadOf` configuration through a `Repository`, so `list` outside a
//! repository bails where stock git succeeds; and a transport failure reports
//! `fatal: unable to access '<url>': <gix error>` — git's shape, but with the
//! `reqwest` error text where libcurl's would be.

use anyhow::{bail, Result};
use std::io::{BufRead, Write};
use std::process::ExitCode;

use gix::bstr::{BStr, ByteSlice};
use gix::protocol::handshake::Ref;

/// `error()`'s prefix plus remote-curl's own tag, used on both failure paths.
const USAGE: &str = "error: remote-curl: usage: git remote-curl <remote> [<url>]";

/// The capability block a curl-backed git 2.55.0 advertises, terminator included.
const CAPABILITIES: &str =
    "stateless-connect\nfetch\nget\noption\npush\ncheck-connectivity\nobject-format\n\n";

/// The subset of remote-curl's `struct options` this port can observe.
///
/// Every other field `set_option()` writes only ever steers `fetch`/`push`,
/// which are unported, so recording them would be dead state.
#[derive(Default)]
struct Options {
    /// `option object-format true` — makes `list` emit the `:object-format` line.
    object_format: bool,
}

/// `git remote-http` — see the module docs for exactly what is and is not ported.
pub fn remote_http(args: &[String]) -> Result<ExitCode> {
    // `main()`'s `if (argc < 2)`; argv[0] is the program name in both layouts.
    if args.len() < 2 {
        eprintln!("{USAGE}");
        return Ok(ExitCode::from(1));
    }

    // `url_in = (argc > 2) ? argv[2] : argv[1]`, then `end_url_with_slash()`.
    let url_in = args.get(2).unwrap_or(&args[1]).as_str();
    let url = if url_in.is_empty() || url_in.ends_with('/') {
        url_in.to_string()
    } else {
        format!("{url_in}/")
    };

    // `credential_from_url(&http_auth, url.buf)`, which runs before stdin is
    // ever read. Its only reachable failure here is a missing scheme.
    if !has_scheme(&url) {
        eprintln!("warning: url has no scheme: {url}");
        eprintln!("fatal: credential url cannot be parsed: {url}");
        return Ok(ExitCode::from(128));
    }

    let mut options = Options::default();
    let mut stdin = std::io::stdin().lock();
    let mut line: Vec<u8> = Vec::new();

    loop {
        line.clear();
        // `strbuf_getline_lf()`: read to LF and strip it; CR is left in place.
        if stdin.read_until(b'\n', &mut line)? == 0 {
            // Unexpected end of the command stream; git returns 1 silently.
            return Ok(ExitCode::from(1));
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        // The blank line that ends a command batch: git returns 0.
        if line.is_empty() {
            return Ok(ExitCode::SUCCESS);
        }

        let cmd = line.as_bstr();
        if cmd == "capabilities" {
            print!("{CAPABILITIES}");
            std::io::stdout().flush()?;
        } else if let Some(arg) = strip_prefix(&line, b"option ") {
            match set_option(arg, &mut options) {
                SetOption::Ok => println!("ok"),
                SetOption::Unsupported => println!("unsupported"),
                SetOption::Invalid => println!("error invalid value"),
                SetOption::Die(msg) => {
                    eprintln!("fatal: {msg}");
                    return Ok(ExitCode::from(128));
                }
            }
            std::io::stdout().flush()?;
        } else if cmd == "list" || strip_prefix(&line, b"list ").is_some() {
            // `cmd_list()`: `for_push = !!strstr(buf->buf + 4, "for-push")`.
            if cmd[4..].contains_str("for-push") {
                bail!(
                    "'list for-push' needs a git-receive-pack service advertisement; the vendored \
                     gitoxide only performs an upload-pack handshake and has no send-pack driver"
                );
            }
            if let Some(code) = list(&url, &options)? {
                return Ok(code);
            }
        } else if strip_prefix(&line, b"fetch ").is_some() {
            bail!(
                "'fetch' must download the named object ids without writing refs; gix's fetch is \
                 refspec-driven and updates refs, and exposes no object-id entry point"
            );
        } else if strip_prefix(&line, b"push ").is_some() {
            bail!(
                "'push' needs a git-receive-pack driver (ref-update commands, pack generation, \
                 report-status parsing); gix-protocol implements only the fetch half"
            );
        } else if cmd == "stateless-connect"
            || strip_prefix(&line, b"stateless-connect ").is_some()
        {
            bail!(
                "'stateless-connect' must proxy raw pkt-lines between stdio and a series of HTTP \
                 POSTs; gix-transport's HTTP client is a typed request/response pair, not a duplex \
                 byte channel"
            );
        } else if strip_prefix(&line, b"get ").is_some() {
            bail!(
                "'get' needs an authenticated HTTP download to a file; gix-transport's HTTP layer \
                 is private to the git services and exposes no general GET API"
            );
        } else {
            // `error("remote-curl: unknown command '%s' from git", buf.buf)`.
            eprintln!("error: remote-curl: unknown command '{cmd}' from git");
            return Ok(ExitCode::from(1));
        }
    }
}

/// `credential_from_url_gently()`'s scheme check: the URL must contain `://`
/// with at least one character of scheme before it.
fn has_scheme(url: &str) -> bool {
    matches!(url.find("://"), Some(at) if at > 0)
}

/// `skip_prefix()` on a byte line.
fn strip_prefix<'a>(line: &'a [u8], prefix: &[u8]) -> Option<&'a BStr> {
    line.strip_prefix(prefix).map(|rest| rest.as_bstr())
}

/// The three return values of remote-curl's `set_option()` (0 / >0 / <0), plus
/// the one arm that dies outright.
enum SetOption {
    Ok,
    Unsupported,
    Invalid,
    Die(String),
}

/// `set_option()`, driven the way the command loop drives it: the option name
/// runs to the first space, and a missing value is `"true"`.
fn set_option(arg: &BStr, options: &mut Options) -> SetOption {
    let (name, value): (&BStr, &BStr) = match arg.find_byte(b' ') {
        Some(at) => (&arg[..at], &arg[at + 1..]),
        None => (arg, BStr::new("true")),
    };

    // Names are ASCII; a non-UTF-8 name can only ever be unsupported.
    let Ok(name) = std::str::from_utf8(name) else {
        return SetOption::Unsupported;
    };

    match name {
        // strtol with a full-consumption check.
        "verbosity" | "depth" => strtol(value),
        // A literal "true"/"false" compare — no yes/on/1 spellings.
        "progress" | "deepen-relative" | "followtags" | "dry-run" | "check-connectivity"
        | "force" | "cloning" | "update-shallow" | "atomic" => boolean(value),
        // Bool plus the third state git's push-certificate option carries.
        "pushcert" => {
            if value == "if-asked" {
                SetOption::Ok
            } else {
                boolean(value)
            }
        }
        // Stored verbatim; every value is accepted.
        "deepen-since" | "deepen-not" | "cas" | "push-option" | "filter" => SetOption::Ok,
        // Sets a flag unconditionally, so even a non-boolean value is accepted.
        "from-promisor" => SetOption::Ok,
        // The only option whose value this port can observe, and the only one
        // that dies instead of reporting a value error.
        "object-format" => {
            if value == "true" {
                options.object_format = true;
                SetOption::Ok
            } else {
                SetOption::Die(format!("unknown value for object-format: {value}"))
            }
        }
        _ => SetOption::Unsupported,
    }
}

/// git's `strtol(value, &end, 10)` acceptance: at least one digit consumed and
/// nothing left over. Leading whitespace and a leading sign are allowed.
fn strtol(value: &BStr) -> SetOption {
    let s = value.to_str_lossy();
    let digits = s.trim_start_matches([' ', '\t', '\n', '\r', '\x0b', '\x0c']);
    let digits = digits.strip_prefix(['+', '-']).unwrap_or(digits);
    if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
        SetOption::Ok
    } else {
        SetOption::Invalid
    }
}

/// remote-curl's boolean: a literal `true` or `false`, nothing else.
fn boolean(value: &BStr) -> SetOption {
    if value == "true" || value == "false" {
        SetOption::Ok
    } else {
        SetOption::Invalid
    }
}

/// `cmd_list()` for the fetch direction: fetch the advertisement and render it
/// through `output_refs()`.
///
/// Returns `None` when the caller should keep reading commands (git's `list`
/// does not end the session), or `Some(code)` for the fatal exit git takes when
/// the remote cannot be reached.
fn list(url: &str, options: &Options) -> Result<Option<ExitCode>> {
    // gix resolves transport, credential and `insteadOf` configuration through a
    // Repository; there is no repository-less remote in the vendored crates.
    let Ok(repo) = gix::discover(".") else {
        bail!("'list' outside a repository is not supported (no repository found)");
    };

    let remote = match repo.find_fetch_remote(Some(BStr::new(url))) {
        Ok(remote) => remote,
        Err(e) => {
            eprintln!("fatal: unable to access '{url}': {e}");
            return Ok(Some(ExitCode::from(128)));
        }
    };

    // `prefix_from_spec_as_filter_on_remote` must be off: the helper lists every
    // advertised ref, not just the ones a refspec would select.
    let ref_map = match remote
        .connect(gix::remote::Direction::Fetch)
        .map_err(|e| e.to_string())
        .and_then(|conn| {
            conn.ref_map(
                gix::progress::Discard,
                gix::remote::ref_map::Options {
                    prefix_from_spec_as_filter_on_remote: false,
                    ..Default::default()
                },
            )
            .map_err(|e| e.to_string())
        }) {
        Ok((map, _handshake)) => map,
        Err(e) => {
            eprintln!("fatal: unable to access '{url}': {e}");
            return Ok(Some(ExitCode::from(128)));
        }
    };

    let mut out = String::new();
    if options.object_format {
        out.push_str(&format!(":object-format {}\n", ref_map.object_hash));
    }
    for r in &ref_map.remote_refs {
        // A symbolic target wins over the object id, as in `output_refs()`.
        match r {
            Ref::Symbolic {
                full_ref_name,
                target,
                ..
            }
            | Ref::Unborn {
                full_ref_name,
                target,
            } => out.push_str(&format!("@{target} {full_ref_name}\n")),
            Ref::Direct {
                full_ref_name,
                object,
            } => out.push_str(&format!("{} {full_ref_name}\n", object.to_hex())),
            // remote-curl keeps the tag object under the ref's own name and
            // folds the peeled id into `->peeled`, which `list` never prints.
            Ref::Peeled {
                full_ref_name, tag, ..
            } => out.push_str(&format!("{} {full_ref_name}\n", tag.to_hex())),
        }
    }
    out.push('\n');
    print!("{out}");
    std::io::stdout().flush()?;

    Ok(None)
}
