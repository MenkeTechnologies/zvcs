//! `git credential-netrc` — netrc/authinfo backed credential helper.
//!
//! Stock git ships this as a Perl script (`contrib/credential/netrc`). Its logic
//! is self-contained — parse netrc-style files, answer the credential-helper
//! protocol on stdin/stdout — so it ports directly. The only gitoxide substrate
//! it needs is a `gpg.program` config lookup, done here through
//! [`gix::Repository::config_snapshot`] with a [`gix::config::File::from_globals`]
//! fallback for invocations outside a repository (stock's `Git::config` reads
//! global config the same way).
//!
//! Covered, byte-for-byte against the stock script's stdout:
//!   * `get` mode — the only mode stock implements. Any other mode exits 0
//!     silently, and a missing mode dies with exit status 255, both as stock does.
//!   * `-f`/`--file <authfile>` (repeatable, ordered), `-g`/`--gpg [<program>]`,
//!     `-k`/`--insecure`, `-d`/`--debug`, `-v`/`--verbose`, `-h`/`--help`.
//!   * Short-option bundling (`-kv`) and attached values (`-fFILE`), matching the
//!     script's `Getopt::Long::Configure("bundling")`.
//!   * Default file list `~/.authinfo.gpg ~/.netrc.gpg ~/.authinfo ~/.netrc`,
//!     `.gpg` files decrypted through `<gpg.program> --decrypt`.
//!   * The Net::Netrc tokenizer (quoting, backslash escapes, `machine`/`default`/
//!     `macdef`, cross-line token continuation), the netrc↔credential token map,
//!     `Git::port_num` promotion of `port` into a `host:port` value, first-match
//!     selection across files, and the sorted `key=value` reply that omits tokens
//!     already supplied in the query.
//!
//! Deliberate divergences, all documented rather than silent:
//!   * Unknown options `bail!` here. Stock warns on stderr and continues, which
//!     silently drops the flag; erroring is the honest behaviour.
//!   * `--help` interpolates this binary's path where stock interpolates the Perl
//!     script's, so that text is not byte-identical.
//!   * The owner check uses [`gix::sec::identity::is_path_owned_by_current_user`]
//!     (effective uid, `symlink_metadata`) where stock compares `stat`'s uid to
//!     the real uid; these differ only under `sudo` or for symlinked authfiles.
//!   * Symbolic netrc ports resolve by parsing `/etc/services` rather than calling
//!     `getservbyname`, since no libc binding is available in this crate.
//!   * An entry carrying both `login` and `user` resolves to the later one. Stock
//!     picks whichever Perl's randomized hash order yields, i.e. it is undefined.
//!   * `-d` emits a reduced subset of stock's debug trace on stderr; `-v` emits
//!     the verbose messages in full. Neither stream is part of the reply.
//!   * Long options must be spelled in full; stock accepts unique prefixes.

use anyhow::{bail, Result};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::process::ExitCode;

use gix::bstr::{BString, ByteSlice};

/// The credential-helper protocol tokens this helper understands, in the sorted
/// order stock iterates them (Perl's `sort keys %$query`).
const CRED_TOKENS: [&str; 5] = ["host", "password", "path", "protocol", "username"];

/// Parsed command line for a single invocation.
struct Opts {
    help: bool,
    debug: bool,
    verbose: bool,
    insecure: bool,
    files: Vec<String>,
    gpg: Option<String>,
}

/// `git credential-netrc` — answer credential queries from netrc/authinfo files.
///
/// See the module docs for the supported flags and the fidelity notes.
pub fn credential_netrc(args: &[String]) -> Result<ExitCode> {
    // `args[0]` is the subcommand name itself, as dispatch hands it over.
    let (opts, rest) = parse_args(args.get(1..).unwrap_or(&[]))?;

    if opts.help {
        print!("{}", help_text());
        return Ok(ExitCode::SUCCESS);
    }

    // Stock: `die "Syntax: ..."` when no mode is given, which Perl turns into
    // exit status 255.
    let Some(mode) = rest.first() else {
        eprintln!("Syntax: {} [(-f <authfile>)...] [-d] get", program_name());
        return Ok(ExitCode::from(255));
    };

    // Only `get` is implemented upstream; every other mode is a silent success.
    if mode != "get" {
        return Ok(ExitCode::SUCCESS);
    }

    let files = if opts.files.is_empty() {
        default_files()
    } else {
        opts.files.clone()
    };

    // Resolved eagerly so the verbose trace matches stock's `load_config` order.
    let gpg = resolve_gpg_program(&opts);
    log_verbose(&opts, format_args!("using {gpg} for GPG operations"));

    let query = read_query_from_stdin(&opts);

    for file in &files {
        let gpg_mode = file.ends_with(".gpg");

        if !is_readable(file) {
            log_verbose(&opts, format_args!("Unable to read {file}; skipping it"));
            continue;
        }

        // The permission/ownership guard Net::Netrc applies, skipped for GPG
        // files (their contents are protected by the encryption) and under -k.
        if !gpg_mode && !opts.insecure {
            match check_secure(file) {
                Insecure::Mode(mode_bits) => {
                    log_verbose(
                        &opts,
                        format_args!("Insecure {file} (mode={mode_bits:04o}); skipping it"),
                    );
                    continue;
                }
                Insecure::NotOwner => {
                    log_verbose(&opts, format_args!("Not owner of {file}; skipping it"));
                    continue;
                }
                Insecure::Ok => {}
            }
        }

        let contents = if gpg_mode {
            log_verbose(
                &opts,
                format_args!("Using GPG to open {file}: [{gpg} --decrypt {file}]"),
            );
            gpg_decrypt(&gpg, file)
        } else {
            log_verbose(&opts, format_args!("Opening {file}..."));
            std::fs::read(file).ok()
        };
        let Some(contents) = contents else {
            log_verbose(&opts, format_args!("Unable to open {file}"));
            continue;
        };

        let entries = load_netrc(&contents, &opts);
        if entries.is_empty() {
            log_verbose(&opts, format_args!("No netrc entries found in {file}"));
            continue;
        }

        if let Some(entry) = find_entry(&entries, &query) {
            print_credential_data(entry, &query)?;
            // Stock stops at the first matching entry across all files.
            break;
        }
    }

    Ok(ExitCode::SUCCESS)
}

/// Split `args` into options and the trailing positionals, honouring the
/// bundling rules stock enables via `Getopt::Long::Configure("bundling")`.
fn parse_args(args: &[String]) -> Result<(Opts, Vec<String>)> {
    let mut opts = Opts {
        help: false,
        debug: false,
        verbose: false,
        insecure: false,
        files: Vec::new(),
        gpg: None,
    };
    let mut rest = Vec::new();

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        i += 1;

        if a == "--" {
            rest.extend(args[i..].iter().cloned());
            break;
        }

        if let Some(long) = a.strip_prefix("--") {
            let (name, attached) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (long, None),
            };
            match name {
                "help" => opts.help = true,
                "debug" => opts.debug = true,
                "verbose" => opts.verbose = true,
                "insecure" => opts.insecure = true,
                "file" => {
                    let v = match attached {
                        Some(v) => v,
                        None => take_value(args, &mut i, a)?,
                    };
                    opts.files.push(v);
                }
                // `gpg|g:s` — the value is optional, so a bare `--gpg` only
                // absorbs the next argument when it does not look like a flag.
                "gpg" => opts.gpg = Some(attached.unwrap_or_else(|| take_optional_value(args, &mut i))),
                _ => bail!(
                    "unsupported flag {a:?} (ported: -f/--file, -g/--gpg, -k/--insecure, -d/--debug, -v/--verbose, -h/--help)"
                ),
            }
            continue;
        }

        if a.len() > 1 && a.starts_with('-') {
            let chars: Vec<char> = a[1..].chars().collect();
            let mut j = 0;
            while j < chars.len() {
                let c = chars[j];
                j += 1;
                match c {
                    'h' => opts.help = true,
                    'd' => opts.debug = true,
                    'v' => opts.verbose = true,
                    'k' => opts.insecure = true,
                    'f' => {
                        // `-fFILE` takes the rest of the cluster, `-f FILE` the
                        // next argument.
                        let tail: String = chars[j..].iter().collect();
                        j = chars.len();
                        let v = if tail.is_empty() {
                            take_value(args, &mut i, a)?
                        } else {
                            tail
                        };
                        opts.files.push(v);
                    }
                    'g' => {
                        let tail: String = chars[j..].iter().collect();
                        j = chars.len();
                        let v = if tail.is_empty() {
                            take_optional_value(args, &mut i)
                        } else {
                            tail
                        };
                        opts.gpg = Some(v);
                    }
                    _ => bail!(
                        "unsupported flag {:?} (ported: -f/--file, -g/--gpg, -k/--insecure, -d/--debug, -v/--verbose, -h/--help)",
                        format!("-{c}")
                    ),
                }
            }
            continue;
        }

        rest.push(a.to_string());
    }

    Ok((opts, rest))
}

/// Consume the next argument as a mandatory option value.
fn take_value(args: &[String], i: &mut usize, flag: &str) -> Result<String> {
    if *i >= args.len() {
        bail!("option {flag:?} requires a value");
    }
    let v = args[*i].clone();
    *i += 1;
    Ok(v)
}

/// Consume the next argument as an optional option value, leaving it in place
/// when it looks like another flag (or when there is nothing left).
fn take_optional_value(args: &[String], i: &mut usize) -> String {
    match args.get(*i) {
        Some(v) if !v.starts_with('-') => {
            *i += 1;
            v.clone()
        }
        _ => String::new(),
    }
}

/// The tilde-expanded default search list, `.gpg` variants first.
fn default_files() -> Vec<String> {
    let Some(home) = std::env::var_os("HOME") else {
        return Vec::new();
    };
    let home = home.to_string_lossy().into_owned();
    [".authinfo.gpg", ".netrc.gpg", ".authinfo", ".netrc"]
        .iter()
        .map(|n| format!("{home}/{n}"))
        .collect()
}

/// `gpg.program`, falling back to `gpg`, mirroring stock's `load_config`.
/// An explicit `-g <program>` wins even when empty, as it does upstream.
fn resolve_gpg_program(opts: &Opts) -> String {
    if let Some(g) = &opts.gpg {
        return g.clone();
    }
    let from_repo = gix::discover(".")
        .ok()
        .and_then(|repo| repo.config_snapshot().string("gpg.program"));
    let configured = match from_repo {
        Some(v) => Some(v),
        // Outside a repository stock still sees system/global config.
        None => gix::config::File::from_globals()
            .ok()
            .and_then(|f| f.string("gpg.program")),
    };
    configured
        .map(|v| v.to_str_lossy().into_owned())
        .unwrap_or_else(|| "gpg".to_string())
}

/// Whether `path` can be opened for reading, as Perl's `-r` tests.
fn is_readable(path: &str) -> bool {
    std::fs::File::open(path).is_ok()
}

/// Outcome of the netrc permission guard.
enum Insecure {
    Ok,
    /// The mode has group/other bits set; carries `mode & 0o7777` for the message.
    Mode(u32),
    NotOwner,
}

#[cfg(unix)]
fn check_secure(path: &str) -> Insecure {
    use std::os::unix::fs::PermissionsExt;

    // A stat failure leaves stock's `@stat` empty, which skips both checks.
    let Ok(meta) = std::fs::metadata(path) else {
        return Insecure::Ok;
    };
    let mode = meta.permissions().mode();
    if mode & 0o77 != 0 {
        return Insecure::Mode(mode & 0o7777);
    }
    match gix::sec::identity::is_path_owned_by_current_user(std::path::Path::new(path)) {
        Ok(false) => Insecure::NotOwner,
        _ => Insecure::Ok,
    }
}

/// Stock skips the guard entirely on Windows, OS/2 and Cygwin.
#[cfg(not(unix))]
fn check_secure(_path: &str) -> Insecure {
    Insecure::Ok
}

/// Run `<gpg> --decrypt <file>` and return its stdout. Stderr is inherited, and
/// a failed spawn or non-zero exit yields whatever was produced (stock reads the
/// pipe regardless and simply finds no entries).
fn gpg_decrypt(gpg: &str, file: &str) -> Option<Vec<u8>> {
    let out = std::process::Command::new(gpg)
        .arg("--decrypt")
        .arg(file)
        .stderr(std::process::Stdio::inherit())
        .output()
        .ok()?;
    Some(out.stdout)
}

/// Read the query from stdin: `token=value` lines whose token is one of the five
/// credential-protocol tokens. Lines with an empty value never match stock's
/// `^([^=]+)=(.+)` and are ignored, as are unknown tokens.
fn read_query_from_stdin(opts: &Opts) -> BTreeMap<&'static str, BString> {
    let mut buf = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut buf);

    let mut query = BTreeMap::new();
    for line in buf.split(|&b| b == b'\n') {
        let Some(eq) = line.iter().position(|&b| b == b'=') else {
            continue;
        };
        // `[^=]+` requires a non-empty token and `(.+)` a non-empty value. A CR
        // from a CRLF line is kept, since Perl's `.` matches it.
        if eq == 0 {
            continue;
        }
        let (tok, val) = (&line[..eq], &line[eq + 1..]);
        if val.is_empty() {
            continue;
        }
        let Some(name) = CRED_TOKENS.iter().find(|n| n.as_bytes() == tok) else {
            continue;
        };
        log_debug(
            opts,
            format_args!(
                "We were given search token {name} and value {}",
                val.as_bstr()
            ),
        );
        query.insert(*name, BString::from(val));
    }
    query
}

/// A netrc entry translated into credential-protocol tokens. `BTreeMap` gives
/// the sorted key order stock's `sort keys %$entry` produces on output.
type Entry = BTreeMap<&'static str, BString>;

/// An entry as it appears in the file, before token translation.
struct RawEntry {
    machine: Option<BString>,
    /// netrc token → value, in first-seen order; a repeated token overwrites in place.
    toks: Vec<(BString, BString)>,
}

impl RawEntry {
    fn set(&mut self, tok: BString, value: BString) {
        match self.toks.iter_mut().find(|(k, _)| *k == tok) {
            Some(slot) => slot.1 = value,
            None => self.toks.push((tok, value)),
        }
    }
}

/// Map a netrc token to its credential-protocol name. The identity entries are
/// the ones stock derives by folding the map's values back over its keys.
fn tmap(tok: &[u8]) -> Option<&'static str> {
    Some(match tok {
        b"port" => "protocol",
        b"machine" => "host",
        b"path" => "path",
        b"login" | b"user" | b"username" => "username",
        b"password" => "password",
        b"protocol" => "protocol",
        b"host" => "host",
        _ => return None,
    })
}

/// Parse `contents` and translate every entry into credential-protocol tokens.
fn load_netrc(contents: &[u8], opts: &Opts) -> Vec<Entry> {
    let mut out = Vec::new();

    for raw in net_netrc_loader(contents, opts) {
        let Some(machine) = raw.machine.clone() else {
            // `default` blocks and a bare trailing `machine` never yield a host.
            continue;
        };

        // A `port` token is validated, converted, and then removed — it is never
        // carried through the map as `protocol`.
        let mut num_port: Option<BString> = None;
        let mut toks = Vec::new();
        for (k, v) in raw.toks {
            if k == "port" {
                match port_num(&v) {
                    Some(n) => num_port = Some(n),
                    None => eprintln!("ignoring invalid port `{}' from netrc file", v.as_bstr()),
                }
                continue;
            }
            toks.push((k, v));
        }

        let mut entry = Entry::new();
        entry.insert("host", machine);
        for (k, v) in toks {
            if let Some(name) = tmap(&k) {
                entry.insert(name, v);
            }
        }

        // `machine X port Y` with a numeric Y becomes the host `X:Y`.
        if let Some(port) = num_port {
            if let Some(host) = entry.get("host").cloned() {
                let mut joined = host.to_vec();
                joined.push(b':');
                joined.extend_from_slice(port.as_slice());
                entry.insert("host", BString::from(joined));
            }
        }

        out.push(entry);
    }

    out
}

/// `Git::port_num`: a decimal string in 1..=65535 is taken verbatim, otherwise
/// the token is resolved as a service name.
fn port_num(port: &[u8]) -> Option<BString> {
    if !port.is_empty() && port.iter().all(|b| b.is_ascii_digit()) {
        if let Ok(n) = port.to_str_lossy().parse::<u64>() {
            if n > 0 && n <= 65535 {
                // Returned verbatim, so `port 08080` keeps its leading zero.
                return Some(BString::from(port));
            }
        }
        return None;
    }
    getservbyname(port).map(|n| BString::from(n.to_string()))
}

/// Resolve a service name through `/etc/services`, matching any protocol — the
/// behaviour of `getservbyname($name, '')`.
fn getservbyname(name: &[u8]) -> Option<u16> {
    let contents = std::fs::read("/etc/services").ok()?;
    for line in contents.split(|&b| b == b'\n') {
        let line = match line.iter().position(|&b| b == b'#') {
            Some(i) => &line[..i],
            None => line,
        };
        let mut fields = line
            .split(|b| b.is_ascii_whitespace())
            .filter(|f| !f.is_empty());
        let (Some(service), Some(port_proto)) = (fields.next(), fields.next()) else {
            continue;
        };
        // The first field is the canonical name; everything after `port/proto`
        // is an alias, and both are matchable.
        if service != name && !fields.any(|alias| alias == name) {
            continue;
        }
        let port = match port_proto.iter().position(|&b| b == b'/') {
            Some(i) => &port_proto[..i],
            None => port_proto,
        };
        if let Ok(n) = port.to_str_lossy().parse::<u16>() {
            return Some(n);
        }
    }
    None
}

/// The extracted Net::Netrc reader: tokenize each line, then interpret the
/// accumulated tokens. Tokens do not carry across lines — the token loop drains
/// per line, so a trailing `machine` with no host yields a host-less entry.
fn net_netrc_loader(contents: &[u8], opts: &Opts) -> Vec<RawEntry> {
    let mut entries: Vec<RawEntry> = Vec::new();
    // Index into `entries` for `machine` blocks; `default` blocks are collected
    // into a scratch entry that is never pushed, exactly as stock drops them.
    let mut cur: Option<usize> = None;
    let mut scratch = RawEntry {
        machine: None,
        toks: Vec::new(),
    };
    let mut have_mach = false;
    let mut macdef = false;

    for line in split_lines(contents) {
        // An empty line closes an open `macdef` body.
        if line.len() == 1 && line[0] == b'\n' {
            macdef = false;
        }
        if macdef {
            continue;
        }

        let toks = tokenize(line);
        let mut it = toks.into_iter().peekable();

        while let Some(first) = it.peek().cloned() {
            if first == "default" {
                it.next();
                scratch = RawEntry {
                    machine: None,
                    toks: Vec::new(),
                };
                cur = None;
                have_mach = true;
                continue;
            }

            let tok = it.next().expect("peeked");

            if tok == "machine" {
                let host = it.next();
                entries.push(RawEntry {
                    machine: host,
                    toks: Vec::new(),
                });
                cur = Some(entries.len() - 1);
                have_mach = true;
            } else if tmap(&tok).is_some() {
                if !have_mach {
                    log_debug(
                        opts,
                        format_args!(
                            "Skipping token {} because no machine was given",
                            tok.as_bstr()
                        ),
                    );
                    continue;
                }
                let Some(mut value) = it.next() else {
                    log_debug(
                        opts,
                        format_args!("Token {} had no value, skipping it.", tok.as_bstr()),
                    );
                    continue;
                };
                // Carried over verbatim from stock: strip the `/\` sequence some
                // netrc writers emit before a backslash.
                value = BString::from(value.replace(b"/\\", b"\\"));
                match cur {
                    Some(idx) => entries[idx].set(tok, value),
                    None => scratch.set(tok, value),
                }
            } else if tok == "macdef" {
                if !have_mach {
                    continue;
                }
                // The macro name is consumed and the body skipped until a blank line.
                it.next();
                macdef = true;
            }
            // Any other token is silently dropped, as upstream does.
        }
    }

    entries
}

/// Split into lines that retain their trailing newline, so the `\A\n\Z` test
/// that closes a `macdef` body sees exactly what Perl's `<$fh>` yields.
fn split_lines(contents: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, &b) in contents.iter().enumerate() {
        if b == b'\n' {
            out.push(&contents[start..=i]);
            start = i + 1;
        }
    }
    if start < contents.len() {
        out.push(&contents[start..]);
    }
    out
}

/// Tokenize one raw line the way Net::Netrc does.
///
/// Leading whitespace is stripped and the trailing newline chomped, then tokens
/// are pulled off the front with
/// `("((?:[^"]+|\\.)*)"|((?:[^\\\s]+|\\.)*))\s*`. A quoted token therefore runs
/// to the *first* following double quote (the greedy `[^"]+` can never cross
/// one), and every token has `\X` collapsed to `X` afterwards.
fn tokenize(line: &[u8]) -> Vec<BString> {
    let mut s = line;
    // s/^\s*// then chomp.
    while let Some((&b, tail)) = s.split_first() {
        if b.is_ascii_whitespace() {
            s = tail;
        } else {
            break;
        }
    }
    if let Some(t) = s.strip_suffix(b"\n") {
        s = t;
    }

    let mut out = Vec::new();
    while !s.is_empty() {
        let (raw, consumed) = match quoted_token(s) {
            Some(hit) => hit,
            None => unquoted_token(s),
        };
        let mut rest = &s[consumed..];
        while let Some((&b, tail)) = rest.split_first() {
            if b.is_ascii_whitespace() {
                rest = tail;
            } else {
                break;
            }
        }
        // Perl spins forever when neither a token nor trailing whitespace is
        // consumed (a lone trailing backslash does this); stop instead.
        if rest.len() == s.len() {
            break;
        }
        out.push(unescape(raw));
        s = rest;
    }
    out
}

/// Match the quoted alternative, returning the inner bytes and the length
/// consumed including both quotes. Fails when there is no closing quote.
fn quoted_token(s: &[u8]) -> Option<(&[u8], usize)> {
    if s.first() != Some(&b'"') {
        return None;
    }
    let end = s[1..].iter().position(|&b| b == b'"')? + 1;
    Some((&s[1..end], end + 1))
}

/// Match the unquoted alternative: runs of non-backslash, non-whitespace bytes
/// interleaved with `\` plus one following byte.
fn unquoted_token(s: &[u8]) -> (&[u8], usize) {
    let mut i = 0;
    while i < s.len() {
        let b = s[i];
        if b == b'\\' {
            if i + 1 < s.len() {
                i += 2;
            } else {
                break;
            }
        } else if b.is_ascii_whitespace() {
            break;
        } else {
            i += 1;
        }
    }
    (&s[..i], i)
}

/// `s/\\(.)/$1/g` — drop each backslash that is followed by another byte.
fn unescape(raw: &[u8]) -> BString {
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'\\' && i + 1 < raw.len() {
            out.push(raw[i + 1]);
            i += 2;
        } else {
            out.push(raw[i]);
            i += 1;
        }
    }
    BString::from(out)
}

/// The first entry satisfying every query token. A token the entry does not
/// carry is not a constraint, and a query token with no value matches anything.
fn find_entry<'a>(
    entries: &'a [Entry],
    query: &BTreeMap<&'static str, BString>,
) -> Option<&'a Entry> {
    entries.iter().find(|entry| {
        CRED_TOKENS.iter().all(|check| match entry.get(check) {
            None => true,
            Some(have) => match query.get(check) {
                Some(want) => want == have,
                None => true,
            },
        })
    })
}

/// Emit the entry's tokens in sorted order, omitting the ones the caller already
/// supplied (the entry matched those by construction).
fn print_credential_data(entry: &Entry, query: &BTreeMap<&'static str, BString>) -> Result<()> {
    let mut out = Vec::new();
    for (token, value) in entry {
        if query.contains_key(token) {
            continue;
        }
        out.extend_from_slice(token.as_bytes());
        out.push(b'=');
        out.extend_from_slice(value.as_slice());
        out.push(b'\n');
    }
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    lock.write_all(&out)?;
    lock.flush()?;
    Ok(())
}

fn log_verbose(opts: &Opts, args: std::fmt::Arguments<'_>) {
    if opts.verbose {
        eprintln!("{args}");
    }
}

fn log_debug(opts: &Opts, args: std::fmt::Arguments<'_>) {
    if opts.debug {
        eprintln!("{args}");
    }
}

/// The path stock interpolates as `$0`. Ours names this binary instead.
fn program_name() -> String {
    std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "git-credential-netrc".to_string())
}

/// The `--help` body, structurally identical to stock's here-doc.
fn help_text() -> String {
    let prog = program_name();
    let short = std::path::Path::new(&prog)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| prog.clone())
        .replace("git-credential-", "");

    format!(
        r#"
{prog} [(-f <authfile>)...] [-g <program>] [-d] [-v] [-k] get

Version 0.2 by tzz@lifelogs.com.  License: BSD.

Options:

  -f|--file <authfile>: specify netrc-style files.  Files with the .gpg
                        extension will be decrypted by GPG before parsing.
                        Multiple -f arguments are OK.  They are processed in
                        order, and the first matching entry found is returned
                        via the credential helper protocol (see below).

                        When no -f option is given, .authinfo.gpg, .netrc.gpg,
                        .authinfo, and .netrc files in your home directory are
                        used in this order.

  -g|--gpg <program>  : specify the program for GPG. By default, this is the
                        value of gpg.program in the git repository or global
                        option or gpg.

  -k|--insecure       : ignore bad file ownership or permissions

  -d|--debug          : turn on debugging (developer info)

  -v|--verbose        : be more verbose (show files and information found)

To enable this credential helper:

  git config credential.helper '{short} -f AUTHFILE1 -f AUTHFILE2'

(Note that Git will prepend "git-credential-" to the helper name and look for it
in the path.)

...and if you want lots of debugging info:

  git config credential.helper '{short} -f AUTHFILE -d'

...or to see the files opened and data found:

  git config credential.helper '{short} -f AUTHFILE -v'

Only "get" mode is supported by this credential helper.  It opens every
<authfile> and looks for the first entry that matches the requested search
criteria:

 'port|protocol':
   The protocol that will be used (e.g., https). (protocol=X)

 'machine|host':
   The remote hostname for a network credential. (host=X)

 'path':
   The path with which the credential will be used. (path=X)

 'login|user|username':
   The credential's username, if we already have one. (username=X)

Thus, when we get this query on STDIN:

host=github.com
protocol=https
username=tzz

this credential helper will look for the first entry in every <authfile> that
matches

machine github.com port https login tzz

OR

machine github.com protocol https login tzz

OR... etc. acceptable tokens as listed above.  Any unknown tokens are
simply ignored.

Then, the helper will print out whatever tokens it got from the entry, including
"password" tokens, mapping back to Git's helper protocol; e.g. "port" is mapped
back to "protocol".  Any redundant entry tokens (part of the original query) are
skipped.

Again, note that only the first matching entry from all the <authfile>s,
processed in the sequence given on the command line, is used.

Netrc/authinfo tokens can be quoted as 'STRING' or "STRING".

No caching is performed by this credential helper.

"#
    )
}
