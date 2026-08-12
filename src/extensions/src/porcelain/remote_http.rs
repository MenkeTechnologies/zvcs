//! `git remote-http` — the smart-HTTP remote helper (stock git's `remote-curl`).
//!
//! This is not a user-facing command: git's transport layer spawns it as
//! `git remote-http <remote> <url>` and drives it over pipes with the
//! remote-helper protocol (`gitremote-helpers(7)`) — one command per line on
//! stdin, a response block terminated by a blank line on stdout.
//!
//! Ported here, byte-verified against git 2.55.0:
//!
//!   * **Argument handling** — [`prologue`], shared verbatim with
//!     `remote-https`, `remote-ftp` and `remote-ftps` because on a stock install
//!     all four are the same executable (`remote-curl.c:1579-1583`: "folding all
//!     the various aliases … since they are all just copies of the same actual
//!     executable"). `argc < 2` prints
//!     `error: remote-curl: usage: git remote-curl <remote> [<url>]` on stderr
//!     and exits 1; the URL is `argv[2]` falling back to the first configured
//!     URL of the remote named by `argv[1]`, arguments past `argv[2]` are
//!     ignored; `end_url_with_slash()` appends a `/` to a non-empty URL that
//!     lacks one; the resulting URL is then run through git's
//!     `credential_from_url_gently()`, which on failure prints
//!     `warning: url has no scheme: <url>` plus
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
//!   * **`get <url> <path>`** — `parse_get()` plus `http.c`'s
//!     `http_get_file()`, which is what `git clone --bundle-uri=https://…`
//!     routes its download through. The argument splits on the *first* space
//!     and a missing space is
//!     `fatal: protocol error: expected '<url> <path>', missing space`; the
//!     body lands in `<path>.temp` opened for append, so an interrupted
//!     download resumes with a `Range: bytes=<len>-` request exactly as
//!     `http_opt_request_remainder()` sets `CURLOPT_RANGE`; on success the
//!     temporary is renamed over `<path>` (`finalize_object_file()`), and the
//!     response block is the single empty line git prints. A failed download
//!     is `fatal: failed to download file at URL '<url>'` with exit 128, and an
//!     unopenable temporary prints `error: Unable to open local file <path>`.
//!     The whole `http.*` family applies, because the request is issued through
//!     the same `reqwest` client the smart transport uses, configured from
//!     `Repository::transport_options()`.
//!
//! Not ported — these bail rather than fabricating a response the caller would
//! act on:
//!
//!   * **`stateless-connect`** — requires proxying raw pkt-lines between stdin
//!     and stdout across a series of HTTP POSTs. `gix-transport`'s HTTP client
//!     exposes a typed `RequestWriter`/`ExtendedBufRead` request-response pair,
//!     not a duplex byte channel that can be bridged to the process's stdio.
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
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::protocol::handshake::Ref;
use gix::protocol::transport::client::blocking_io::http::{reqwest::Remote, Http};

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
    let url = match prologue(args) {
        Prologue::Exit(code) => return Ok(code),
        Prologue::Url(url) => url,
    };
    // The transport below takes a `&str`; a URL that is not UTF-8 has no path
    // through `reqwest`'s `Url` and fails the same way whichever spelling of the
    // bytes reaches it.
    let url = url.to_string();

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
                crate::git_fatal!(
                    "'list for-push' needs a git-receive-pack service advertisement; the vendored \
                     gitoxide only performs an upload-pack handshake and has no send-pack driver"
                );
            }
            if let Some(code) = list(&url, &options)? {
                return Ok(code);
            }
        } else if strip_prefix(&line, b"fetch ").is_some() {
            crate::git_fatal!(
                "'fetch' must download the named object ids without writing refs; gix's fetch is \
                 refspec-driven and updates refs, and exposes no object-id entry point"
            );
        } else if strip_prefix(&line, b"push ").is_some() {
            crate::git_fatal!(
                "'push' needs a git-receive-pack driver (ref-update commands, pack generation, \
                 report-status parsing); gix-protocol implements only the fetch half"
            );
        } else if cmd == "stateless-connect"
            || strip_prefix(&line, b"stateless-connect ").is_some()
        {
            crate::git_fatal!(
                "'stateless-connect' must proxy raw pkt-lines between stdio and a series of HTTP \
                 POSTs; gix-transport's HTTP client is a typed request/response pair, not a duplex \
                 byte channel"
            );
        } else if let Some(arg) = strip_prefix(&line, b"get ") {
            if let Some(code) = parse_get(arg)? {
                return Ok(code);
            }
        } else {
            // `error("remote-curl: unknown command '%s' from git", buf.buf)`.
            eprintln!("error: remote-curl: unknown command '{cmd}' from git");
            return Ok(ExitCode::from(1));
        }
    }
}

/// What `remote-curl.c::cmd_main` has decided by the time it first reads stdin.
pub(super) enum Prologue {
    /// The helper is finished before the command loop: a usage error (1) or the
    /// `die()` in `credential_from_url()` (128).
    Exit(ExitCode),
    /// The URL to serve, carrying the trailing slash `end_url_with_slash()`
    /// guarantees.
    Url(BString),
}

/// `remote-curl.c::cmd_main`'s prologue: the argument check, the URL it settles
/// on, and the credential parse that runs before the first command is read.
///
/// Shared by all four helper spellings because stock git ships one binary for
/// them — `git-remote-https`, `git-remote-ftp` and `git-remote-ftps` are links
/// to `git-remote-http`, and `remote-curl.c` folds their names together
/// (`trace2_cmd_name("remote-curl")`).
///
/// `args` is argv **without** the program name, which is what dispatch hands a
/// porcelain: C's `argc < 2` is therefore an empty list here, C's `argv[1]` is
/// `args[0]`, and C's `argv[2]` is `args[1]`.
pub(super) fn prologue(args: &[String]) -> Prologue {
    // `if (argc < 2) { error(...); goto cleanup; }` with `ret` still 1.
    let Some(name) = args.first() else {
        eprintln!("{USAGE}");
        return Prologue::Exit(ExitCode::from(1));
    };

    // `if (argc > 2) end_url_with_slash(&url, argv[2]); else
    //  end_url_with_slash(&url, remote->url.v[0]);` — surplus arguments past
    // argv[2] are ignored, so they are ignored here too.
    let mut url: Vec<u8> = match args.get(1) {
        Some(explicit) => explicit.clone().into_bytes(),
        None => remote_url(name),
    };
    // `strbuf_complete(&buf, '/')`, which leaves an *empty* URL empty.
    if !url.is_empty() && url.last() != Some(&b'/') {
        url.push(b'/');
    }

    // `http_init()` -> `credential_from_url()`, which dies on a URL it cannot
    // parse after warning about the offending component. This runs before stdin
    // is read, so it is reachable without a server.
    if !credential_url_is_parseable(&url) {
        eprintln!("fatal: credential url cannot be parsed: {}", url.as_bstr());
        return Prologue::Exit(ExitCode::from(128));
    }

    Prologue::Url(url.into())
}

/// `remote_get(argv[1])` reduced to what this command uses: the first
/// `remote.<name>.url` when the argument names a configured remote, the
/// argument itself otherwise, with `url.<base>.insteadOf` applied to the result.
///
/// Values are carried as raw bytes rather than through `gix_url::Url` so the
/// string handed to the credential parser is the configured one verbatim —
/// parsing and re-serialising could normalise it and change the diagnostics.
fn remote_url(name: &str) -> Vec<u8> {
    let Ok(repo) = gix::discover(".") else {
        return name.as_bytes().to_vec();
    };
    let config = repo.config_snapshot();
    let file = config.plumbing();

    let configured = file
        .strings_by("remote", name, "url")
        .and_then(|urls| urls.into_iter().next());
    let url: BString = configured.unwrap_or_else(|| name.into());

    // alias_url(): among all url.<base>.insteadOf values that prefix the URL,
    // the longest one wins and is replaced by its <base>.
    let mut best: Option<(BString, usize)> = None;
    if let Some(sections) = file.sections_by_name("url") {
        for section in sections {
            let Some(base) = section.header().subsection_name() else {
                continue;
            };
            for prefix in section.values("insteadOf") {
                if url.starts_with(prefix.as_slice())
                    && best.as_ref().is_none_or(|(_, len)| prefix.len() > *len)
                {
                    best = Some((base.to_owned(), prefix.len()));
                }
            }
        }
    }

    match best {
        Some((base, len)) => {
            let mut out: Vec<u8> = base.into();
            out.extend_from_slice(&url[len..]);
            out
        }
        None => url.into(),
    }
}

/// `credential.c::credential_from_url_1(c, url, allow_partial = 0, quiet = 0)`,
/// reduced to its observable effect here: the warnings it prints and whether it
/// failed. Returns false once a warning has been emitted.
pub(super) fn credential_url_is_parseable(url: &[u8]) -> bool {
    // Match one of:
    //   (1) proto://<host>/...
    //   (2) proto://<user>@<host>/...
    //   (3) proto://<user>:<pass>@<host>/...
    let Some(proto_end) = url.find("://") else {
        warn_no_scheme(url);
        return false;
    };
    if proto_end == 0 {
        warn_no_scheme(url);
        return false;
    }

    let cp = &url[proto_end + 3..];
    let at = cp.find_byte(b'@');
    let colon = cp.find_byte(b':');
    // strchrnul(cp, '/'): the end of the string when there is no slash.
    let slash = cp.find_byte(b'/').unwrap_or(cp.len());

    let (username, password, host) = match at {
        // Case (1): no credentials in the authority.
        Some(at) if slash <= at => (None, None, &cp[..slash]),
        None => (None, None, &cp[..slash]),
        Some(at) => match colon {
            // Case (3): user and password — the C's `!(!colon || at <= colon)`.
            Some(colon) if colon < at => (
                Some(url_decode(&cp[..colon])),
                Some(url_decode(&cp[colon + 1..at])),
                &cp[at + 1..slash],
            ),
            // Case (2): user only.
            _ => (Some(url_decode(&cp[..at])), None, &cp[at + 1..slash]),
        },
    };

    // The protocol is taken verbatim; every other component is percent-decoded.
    let protocol = &url[..proto_end];
    let host = url_decode(host);

    // "Trim leading and trailing slashes from path" — an all-slash tail leaves
    // no path component at all.
    let mut path = &cp[slash..];
    while path.first() == Some(&b'/') {
        path = &path[1..];
    }
    let path = if path.is_empty() {
        None
    } else {
        let mut decoded = url_decode(path);
        while decoded.len() > 1 && decoded.last() == Some(&b'/') {
            decoded.pop();
        }
        Some(decoded)
    };

    // check_url_component() in the C's order.
    let components: [(&str, Option<&[u8]>); 5] = [
        ("username", username.as_deref()),
        ("password", password.as_deref()),
        ("protocol", Some(protocol)),
        ("host", Some(host.as_slice())),
        ("path", path.as_deref()),
    ];
    for (name, value) in components {
        // C strings: everything past a NUL is invisible to strchr().
        let Some(value) = value else { continue };
        let value = &value[..value.find_byte(0).unwrap_or(value.len())];
        if value.contains(&b'\n') {
            eprintln!(
                "warning: url contains a newline in its {name} component: {}",
                url.as_bstr()
            );
            return false;
        }
    }
    true
}

/// The `warning()` shared by both no-scheme rejections.
fn warn_no_scheme(url: &[u8]) {
    eprintln!("warning: url has no scheme: {}", url.as_bstr());
}

/// `url.c::url_decode_mem` — `%XX` is folded to a byte, and an escape that is
/// not two hex digits is passed through unchanged (which is why a URL such as
/// `https://ho%zzst/r` is accepted by git rather than rejected).
pub(super) fn url_decode(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if input[i] == b'%' && i + 2 < input.len() {
            if let (Some(hi), Some(lo)) = (hex(input[i + 1]), hex(input[i + 2])) {
                out.push(hi << 4 | lo);
                i += 3;
                continue;
            }
        }
        out.push(input[i]);
        i += 1;
    }
    out
}

/// One hex digit, or `None` for anything else — the C's `hexval()`.
fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// `parse_get()`: split `<url> <path>` on the first space, download, then print
/// the empty line that terminates the command's response block.
///
/// Returns `Some(code)` for git's two `die()` paths and `None` to keep the
/// command loop running, matching how `list` reports its outcome.
pub(super) fn parse_get(arg: &BStr) -> Result<Option<ExitCode>> {
    // `space = strchr(arg, ' '); if (!space) die(...)`.
    let Some(space) = arg.find_byte(b' ') else {
        eprintln!("fatal: protocol error: expected '<url> <path>', missing space");
        return Ok(Some(ExitCode::from(128)));
    };
    let (url, path) = (&arg[..space], &arg[space + 1..]);

    // Both halves reach libcurl and the filesystem as C strings; a non-UTF-8
    // spelling has no path through `reqwest`'s `Url` or `Path` on any platform
    // this builds for, and git's own failure for an unusable URL is the same
    // `die()` below.
    let (Ok(url), Ok(path)) = (url.to_str(), path.to_str()) else {
        eprintln!("fatal: failed to download file at URL '{}'", url.to_str_lossy());
        return Ok(Some(ExitCode::from(128)));
    };

    if http_get_file(url, Path::new(path)).is_err() {
        eprintln!("fatal: failed to download file at URL '{url}'");
        return Ok(Some(ExitCode::from(128)));
    }

    println!();
    std::io::stdout().flush()?;
    Ok(None)
}

/// `http.c`'s `http_get_file()`: download `url` into `filename` by way of a
/// `<filename>.temp` staging file that a later run can resume from.
///
/// The request is issued through the same `reqwest` client the smart transport
/// uses, configured from the repository's `http.*` settings, so proxy, TLS,
/// timeout, redirect and extra-header configuration all apply here as they do
/// to a fetch. Outside a repository the client keeps its defaults, which is the
/// closest reachable analogue of git reading only its system/global config.
///
/// Server authentication is the one part of `http_request_recoverable()` that
/// is not reproduced: a `401` fails the download instead of consulting the
/// credential helper and retrying. The credential plumbing lives on
/// `gix-transport`'s service-level `Transport`, not on the raw client this
/// needs, so a bundle URI behind HTTP auth is out of reach here.
fn http_get_file(url: &str, filename: &Path) -> Result<(), ()> {
    // `strbuf_addf(&tmpfile, "%s.temp", filename)`.
    let mut tmpfile = PathBuf::from(filename).into_os_string();
    tmpfile.push(".temp");
    let tmpfile = PathBuf::from(tmpfile);

    // `fopen(tmpfile.buf, "a")`: append, creating on demand.
    let mut file = match std::fs::OpenOptions::new().append(true).create(true).open(&tmpfile) {
        Ok(file) => file,
        Err(_) => {
            eprintln!("error: Unable to open local file {}", tmpfile.display());
            return Err(());
        }
    };

    // `off_t posn = ftello(result)` — a file opened for append starts at its end,
    // so a leftover temporary from an interrupted download is resumed rather than
    // restarted.
    let posn = file.metadata().map(|m| m.len()).unwrap_or(0);

    let mut http = Remote::default();
    if let Ok(repo) = gix::discover(".") {
        if let Ok(Some(options)) = repo.transport_options(url, None) {
            http.configure(&*options).ok();
        }
    }

    // `http_opt_request_remainder()` sets `CURLOPT_RANGE` to `<posn>-`, which
    // curl sends as a byte range.
    let mut headers: Vec<String> = Vec::new();
    if posn > 0 {
        headers.push(format!("Range: bytes={posn}-"));
    }

    // The base URL is the request URL itself: there is no service tail to swap
    // out when a redirect moves the request, unlike the smart transport's
    // `info/refs?service=…`.
    let mut response = http.get(url, url, headers.iter().map(String::as_str)).map_err(|_| ())?;

    // The headers must be drained before the body: the backend writes them into a
    // zero-capacity pipe and reports a non-success status as an error on it.
    let mut header_block = Vec::new();
    response.headers.read_to_end(&mut header_block).map_err(|_| ())?;

    std::io::copy(&mut response.body, &mut file).map_err(|_| ())?;
    drop(file);

    // `finalize_object_file()`, whose reachable effect here is moving the
    // completed temporary into place.
    std::fs::rename(&tmpfile, filename).map_err(|_| ())
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
