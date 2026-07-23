//! `git remote-ftps` — the FTPS remote helper.
//!
//! `git-remote-ftps` is not a command of its own: it is one of the names under
//! which git installs the single `remote-curl` executable (`git-remote-http`,
//! `-https`, `-ftp`, `-ftps` are all the same binary), so its behaviour is that
//! of `remote-curl.c` — a remote helper that speaks the helper protocol on
//! stdin/stdout and performs the actual transfer with libcurl.
//!
//! ### Covered — byte-identical stdout/stderr and exit code against stock git
//!
//! * the argument prologue: `argc < 2` prints
//!   `error: remote-curl: usage: git remote-curl <remote> [<url>]` and exits 1;
//!   otherwise `argv[2]` is the URL when present, and with only `<remote>` the
//!   URL is git's `remote_get(argv[1])` result — the configured
//!   `remote.<name>.url` (first value) when the name is a configured remote,
//!   the name itself otherwise, with `url.<base>.insteadOf` applied in either
//!   case. `argv[3]` and beyond are ignored, exactly as the C ignores them.
//! * `end_url_with_slash()` — the URL always gets a trailing `/`.
//! * `http_init()`'s URL validation, i.e. `credential_from_url()`: the
//!   `warning: url has no scheme: <url>` and
//!   `warning: url contains a newline in its <component> component: <url>`
//!   diagnostics (components checked in the C's order: username, password,
//!   protocol, host, path, with `%XX` decoding for all but the protocol),
//!   followed by `fatal: credential url cannot be parsed: <url>` and exit 128.
//! * the command loop over stdin, which reads LF-delimited lines (a `\r` is
//!   *not* stripped) and compares them as C strings (a NUL truncates):
//!   `capabilities` emits the seven capability lines plus the terminating blank
//!   line; an empty line ends the loop with exit 0; EOF exits 1; anything
//!   unrecognised prints `error: remote-curl: unknown command '<line>' from git`
//!   and exits 1.
//! * `option <name> [<value>]` in full: the missing value defaults to `true`,
//!   the name is matched as a *prefix* of the option table in source order (so
//!   `option d 5` sets `depth`), and the answer is `ok`, `error invalid value`
//!   or `unsupported`. `option object-format <v>` with `<v>` other than `true`
//!   dies with `fatal: unknown value for object-format: <v>` and exit 128.
//!
//! ### Not covered — these bail rather than fabricate a transfer
//!
//! The commands that move data — `list`, `list for-push`, `fetch <sha1> <ref>`,
//! `push <refspec>`, `get <url> <path>` and `stateless-connect <service>` — all
//! require an FTPS client. The vendored gitoxide has none: `gix_url::Scheme`
//! (src/ported/gix-url/src/scheme.rs:6) knows only `File`, `Git`, `Ssh`, `Http`,
//! `Https` and `Ext`, and `gix-transport` has no FTP connection type, so there
//! is no substrate to speak FTP/FTPS over TLS, walk a dumb remote's
//! `info/refs`, or run the stateless smart protocol against one. They report
//! that instead of printing a plausible-looking ref list.
//!
//! Also not covered: outside a repository the URL lookup for the two-argument
//! form falls back to the given name verbatim — gitoxide reaches configuration
//! through a `Repository`, so global/system `insteadOf` rules are not consulted
//! there (inside a repository they are).

use anyhow::{bail, Result};
use std::io::{BufRead, Write};
use std::process::ExitCode;

use gix::bstr::{BString, ByteSlice};

/// `remote-curl.c`'s usage `error()`, byte-for-byte.
const USAGE: &str = "error: remote-curl: usage: git remote-curl <remote> [<url>]";

/// The reply to `capabilities`, including the terminating empty line.
const CAPABILITIES: &str = "\
stateless-connect
fetch
get
option
push
check-connectivity
object-format

";

/// What an `option <name> <value>` does with its value.
#[derive(Clone, Copy)]
enum Value {
    /// `strtol(value, &end, 10)` must consume the whole string.
    Number,
    /// Literally `true` or `false`; git uses `strcmp`, not `git_parse_bool`.
    Bool,
    /// `true`, `false` or `if-asked`.
    PushCert,
    /// The value is stored (or merely flags the option on) and never validated.
    Any,
    /// Only `true` is accepted; anything else is a `die()`.
    ObjectFormat,
}

/// `remote-curl.c::set_option`'s table, in source order — the order matters
/// because a name is matched as a prefix, so the first entry it prefixes wins.
///
/// The order is pinned by stock git's answers to ambiguous prefixes:
/// `option de abc` is `error invalid value` (so `depth` precedes the `deepen-*`
/// entries), `option dee abc` is `ok` (so `deepen-since` precedes
/// `deepen-relative`), `option p abc` is `error invalid value` (so `progress`
/// precedes `pushcert`), `option push abc` is `error invalid value` (so
/// `pushcert` precedes `push-option`), and `option f abc` is
/// `error invalid value` (so `followtags` precedes `filter`).
const OPTIONS: &[(&str, Value)] = &[
    ("verbosity", Value::Number),
    ("progress", Value::Bool),
    ("depth", Value::Number),
    ("deepen-since", Value::Any),
    ("deepen-not", Value::Any),
    ("deepen-relative", Value::Bool),
    ("followtags", Value::Bool),
    ("dry-run", Value::Bool),
    ("check-connectivity", Value::Bool),
    ("cloning", Value::Bool),
    ("update-shallow", Value::Bool),
    ("pushcert", Value::PushCert),
    ("push-option", Value::Any),
    ("force", Value::Bool),
    ("from-promisor", Value::Any),
    ("filter", Value::Any),
    ("atomic", Value::Bool),
    ("object-format", Value::ObjectFormat),
    ("refetch", Value::Any),
];

/// The subset this port implements, quoted in every rejection message.
const PORTED: &str = "ported: capabilities, option, and the argument/URL prologue";

/// `git remote-ftps <remote> [<url>]` — see the module docs for what is covered.
pub fn remote_ftps(args: &[String]) -> Result<ExitCode> {
    // `args` mirrors C's `argv`: args[0] is the command name, args[1] the
    // remote, args[2] the optional URL. `remote-curl.c` only rejects `argc < 2`;
    // surplus arguments are silently ignored, so they are ignored here too.
    if args.len() < 2 {
        eprintln!("{USAGE}");
        return Ok(ExitCode::from(1));
    }

    let mut url: Vec<u8> = match args.get(2) {
        Some(explicit) => explicit.clone().into_bytes(),
        None => remote_url(&args[1]),
    };
    // end_url_with_slash(): a trailing slash is added if there isn't one.
    if url.last() != Some(&b'/') {
        url.push(b'/');
    }

    // http_init() -> credential_from_url(), which dies on a URL it cannot parse
    // after warning about the offending component.
    if !credential_url_is_parseable(&url) {
        eprintln!("fatal: credential url cannot be parsed: {}", url.as_bstr());
        return Ok(ExitCode::from(128));
    }

    command_loop()
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
fn credential_url_is_parseable(url: &[u8]) -> bool {
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
/// `ftps://ho%zzst/r` is accepted by git rather than rejected).
fn url_decode(input: &[u8]) -> Vec<u8> {
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

/// `remote-curl.c::cmd_main`'s command loop.
///
/// The C initialises its return value to 1 and only clears it when the loop is
/// left through an empty line, so both EOF and an unknown command exit 1.
fn command_loop() -> Result<ExitCode> {
    let stdin = std::io::stdin();
    let mut stdin = stdin.lock();
    let mut stdout = std::io::stdout();

    loop {
        let Some(line) = getline_lf(&mut stdin)? else {
            // EOF without ferror(): git returns quietly with 1.
            return Ok(ExitCode::from(1));
        };
        // Every comparison below is on a C string, so a NUL ends the command.
        let line = &line[..line.iter().position(|&b| b == 0).unwrap_or(line.len())];

        if line.is_empty() {
            return Ok(ExitCode::SUCCESS);
        }

        // The dispatch order and the exact prefixes are the C's: a bare `fetch`,
        // `push`, `get`, `stateless-connect` or `option` (no trailing space and
        // no argument) is an unknown command, while `list` matches both bare and
        // with arguments.
        if let Some(arg) = strip_prefix(line, b"fetch ") {
            bail!(
                "remote-curl: 'fetch {}' needs an FTPS client; the vendored gitoxide has no FTP transport ({PORTED})",
                arg.as_bstr()
            );
        } else if let Some(arg) = strip_prefix(line, b"push ") {
            bail!(
                "remote-curl: 'push {}' needs an FTPS client; the vendored gitoxide has no FTP transport ({PORTED})",
                arg.as_bstr()
            );
        } else if let Some(arg) = strip_prefix(line, b"option ") {
            match set_option(arg) {
                Some(answer) => {
                    stdout.write_all(answer.as_bytes())?;
                    stdout.flush()?;
                }
                // die() from `object-format`: the value follows the option name.
                None => {
                    let value = option_value(arg).1;
                    eprintln!("fatal: unknown value for object-format: {}", value.as_bstr());
                    return Ok(ExitCode::from(128));
                }
            }
        } else if line == b"capabilities".as_slice() {
            stdout.write_all(CAPABILITIES.as_bytes())?;
            stdout.flush()?;
        } else if line == b"list".as_slice() || strip_prefix(line, b"list ").is_some() {
            bail!("remote-curl: 'list' needs an FTPS client to read the remote's refs; the vendored gitoxide has no FTP transport ({PORTED})");
        } else if strip_prefix(line, b"stateless-connect ").is_some() {
            bail!("remote-curl: 'stateless-connect' needs an FTPS client; the vendored gitoxide has no FTP transport ({PORTED})");
        } else if strip_prefix(line, b"get ").is_some() {
            bail!("remote-curl: 'get' needs an FTPS client to download the file; the vendored gitoxide has no FTP transport ({PORTED})");
        } else {
            eprintln!(
                "error: remote-curl: unknown command '{}' from git",
                line.as_bstr()
            );
            return Ok(ExitCode::from(1));
        }
    }
}

/// `skip_prefix()` on bytes.
fn strip_prefix<'a>(line: &'a [u8], prefix: &[u8]) -> Option<&'a [u8]> {
    line.strip_prefix(prefix)
}

/// Split `option `'s argument into `(name, value)` the way `cmd_main` does: the
/// first space separates them, and a name with no space at all gets `true`.
fn option_value(arg: &[u8]) -> (&[u8], &[u8]) {
    match arg.find_byte(b' ') {
        Some(sp) => (&arg[..sp], &arg[sp + 1..]),
        None => (arg, b"true"),
    }
}

/// `remote-curl.c::set_option` plus `cmd_main`'s rendering of its result.
///
/// Returns the exact line to print, or `None` for the one `die()` path
/// (`object-format` with a value other than `true`).
fn set_option(arg: &[u8]) -> Option<&'static str> {
    let (name, value) = option_value(arg);

    // strncmp(name, entry, namelen) == 0 is "entry starts with name", which for
    // an empty name matches the very first entry.
    let matched = OPTIONS
        .iter()
        .find(|(entry, _)| entry.as_bytes().starts_with(name));
    let Some((_, kind)) = matched else {
        return Some("unsupported\n");
    };

    let is = |expected: &[u8]| value == expected;
    let ok = match kind {
        Value::Number => is_strtol(value),
        Value::Bool => is(b"true") || is(b"false"),
        Value::PushCert => is(b"true") || is(b"false") || is(b"if-asked"),
        Value::Any => true,
        Value::ObjectFormat => return is(b"true").then_some("ok\n"),
    };
    Some(if ok { "ok\n" } else { "error invalid value\n" })
}

/// `strtol(value, &end, 10)` followed by the C's `if (value == end || *end)`
/// rejection: optional leading whitespace and sign, at least one digit, and
/// nothing after the digits.
fn is_strtol(value: &[u8]) -> bool {
    let mut i = 0;
    while i < value.len() && matches!(value[i], b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
        i += 1;
    }
    if matches!(value.get(i), Some(b'+') | Some(b'-')) {
        i += 1;
    }
    let digits = i;
    while i < value.len() && value[i].is_ascii_digit() {
        i += 1;
    }
    i > digits && i == value.len()
}

/// `strbuf_getline_lf()` — read up to and including the next LF, then drop the
/// LF. A `\r` is deliberately kept: only `strbuf_getline()` strips it, and this
/// command does not use it. `None` is a clean EOF with nothing read.
fn getline_lf(stdin: &mut impl BufRead) -> std::io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    if stdin.read_until(b'\n', &mut line)? == 0 {
        return Ok(None);
    }
    if line.last() == Some(&b'\n') {
        line.pop();
    }
    Ok(Some(line))
}

#[cfg(test)]
mod tests {
    use super::{is_strtol, option_value, set_option, url_decode};

    #[test]
    fn option_names_are_matched_as_prefixes_in_source_order() {
        // Full names behave as documented in gitremote-helpers(7).
        assert_eq!(set_option(b"verbosity 3"), Some("ok\n"));
        assert_eq!(set_option(b"verbosity x"), Some("error invalid value\n"));
        assert_eq!(set_option(b"followtags yes"), Some("error invalid value\n"));
        assert_eq!(set_option(b"pushcert if-asked"), Some("ok\n"));
        assert_eq!(set_option(b"servpath /x"), Some("unsupported\n"));

        // Abbreviations resolve to the first table entry they prefix, which is
        // how stock git answers each of these.
        assert_eq!(set_option(b"de abc"), Some("error invalid value\n")); // depth
        assert_eq!(set_option(b"dee abc"), Some("ok\n")); // deepen-since
        assert_eq!(set_option(b"push abc"), Some("error invalid value\n")); // pushcert
        assert_eq!(set_option(b"fi abc"), Some("ok\n")); // filter
        // An empty name matches everything, so it lands on `verbosity` and the
        // defaulted value `true` fails its strtol().
        assert_eq!(set_option(b""), Some("error invalid value\n"));

        // A missing value defaults to "true".
        assert_eq!(option_value(b"progress"), (&b"progress"[..], &b"true"[..]));
        assert_eq!(set_option(b"progress"), Some("ok\n"));

        // object-format is the only die() path.
        assert_eq!(set_option(b"object-format true"), Some("ok\n"));
        assert_eq!(set_option(b"object-format sha256"), None);
    }

    #[test]
    fn strtol_and_url_decode_edge_cases() {
        assert!(is_strtol(b"0") && is_strtol(b"-1") && is_strtol(b" 5"));
        assert!(!is_strtol(b"5x") && !is_strtol(b"") && !is_strtol(b"true"));

        assert_eq!(url_decode(b"a%2Fb"), b"a/b".to_vec());
        // A malformed escape survives untouched, as it does in git.
        assert_eq!(url_decode(b"ho%zzst"), b"ho%zzst".to_vec());
        assert_eq!(url_decode(b"x%0a"), b"x\n".to_vec());
    }
}
