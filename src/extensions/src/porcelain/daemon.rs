//! `git daemon` — the `git://` protocol server.
//! **The server itself is not ported: every path that would serve a client
//! bails.**
//!
//! `daemon` binds TCP port 9418, accepts connections, reads a `git-upload-pack`
//! / `git-upload-archive` / `git-receive-pack` request line, and runs that
//! service against a repository. What is ported here is the surface that is
//! byte-verifiable *without* speaking the protocol: the whole command-line
//! parser, every startup check that runs before the listen loop, and
//! `socksetup()` — the bind itself, whose failure is what `git daemon` on an
//! occupied port reports. All of it was checked against git 2.55.0 on Darwin.
//!
//! `daemon` uses a hand-rolled argument loop, not `parse_options`, so its
//! diagnostics differ from most git commands — there is no `error: unknown
//! option` line, no abbreviation matching, and no option-value-in-next-argv
//! form. Reproduced exactly:
//!
//!   * `-h`, any unrecognised `-…` argument, and any option given in a form the
//!     loop does not match (`--strict-paths=x`, bare `--timeout`, `--port=abc`)
//!     → the 647-byte usage block on **stderr**, exit 129. `daemon` has no
//!     special case for `-h`; it falls through to the same branch.
//!   * `--timeout=`, `--init-timeout=`, `--max-connections=` value diagnostics,
//!     which use git's `strtoul_ui`/`strtol_i` and so reject a `-` anywhere, a
//!     trailing non-digit, and out-of-range values:
//!     `fatal: invalid timeout '<v>', expecting a non-negative integer`,
//!     the `init-timeout` variant, and
//!     `fatal: invalid max-connections '<v>', expecting an integer` — exit 128.
//!   * `--enable=`, `--disable=`, `--allow-override=`, `--forbid-override=`
//!     against the three-entry service table → `fatal: No such service <name>`,
//!     exit 128. This fires during parsing, so it precedes every startup check.
//!   * `--log-destination=<d>` outside `stderr|syslog|none` →
//!     `fatal: unknown log destination '<d>'`, exit 128.
//!   * The startup checks, in git's order:
//!     `--detach, --user and --group are incompatible with --inetd`,
//!     `--listen= and --port= are incompatible with --inetd`,
//!     `--group supplied without --user`,
//!     `option --strict-paths requires '<directory>' arguments`,
//!     `base-path '<p>' does not exist or is not a directory`.
//!   * `serve()`'s socket setup: the wildcard bind (`::` then `0.0.0.0`) or one
//!     bind per `--listen=`, each failure logged as
//!     `[<pid>] Could not bind to <addr>: <strerror>` and tolerated, and
//!     `fatal: unable to allocate any listen sockets on port <n>` (exit 128)
//!     only when every address failed — the exit `git daemon` takes when the
//!     port is already in use. The sockets are closed again immediately: there
//!     is nothing to serve on them, and holding the port would lock out a real
//!     daemon.
//!   * The die-routine swap. When the effective log destination is `syslog`
//!     (`--syslog`, or the default under `--inetd`/`--detach`), `daemon`
//!     installs `daemon_die`, which logs to syslog and exits **1** — so those
//!     same startup failures produce an empty stderr and exit 1 instead of
//!     `fatal:` and exit 128. `--log-destination=none` does *not* swap the
//!     routine; only `syslog` does. Last `--syslog`/`--log-destination=` wins.
//!
//! The syslog text itself is not reproduced — there is no syslog binding in the
//! vendored crates. stdout, stderr and the exit code match; the syslog record
//! does not.
//!
//! NOT ported — these `bail!` instead of pretending to have run:
//!
//!   1. **Serving.** There is no server-side protocol implementation in the
//!      vendored crates. `src/ported/gix-transport/src/` contains only
//!      `client/`; `gix-protocol/src/` is `handshake`, `fetch`, `ls_refs` and
//!      `command`, all of it the client talking to a remote, and its
//!      `Cargo.toml` gates everything behind `blocking-client`/`async-client`
//!      with no server feature. Nothing implements the serving side of
//!      `want`/`have`/`ACK`/`NAK`, nothing turns a negotiated set into a
//!      side-band-multiplexed pack, and nothing parses the daemon request line
//!      or its `host=`/`extra` NUL-separated arguments.
//!   2. **The accept loop.** `poll()` over the listen sockets, forking a child
//!      per connection, `--max-connections` child reaping, `--timeout`/
//!      `--init-timeout` alarm handling, `--detach` daemonisation, `--pid-file`
//!      and `--inetd` stdin/stdout service are process-level work with no
//!      substrate in gitoxide, which is a repository-format library — and each
//!      of them exists only to reach the serving code in (1), which does not.
//!   3. **`--user=`/`--group=` privilege drop.** `getpwnam(3)`/`getgrnam(3)`
//!      are not called: they are POSIX identity lookups this port has no use
//!      for while it cannot serve. The lookup cannot be faked from
//!      `/etc/passwd` either —
//!      Darwin resolves users through Directory Services, so a file scan would
//!      report `user not found` for users that exist. `--group` without
//!      `--user` needs no lookup and is checked faithfully; a present `--user`
//!      bails at exactly the point git would call `getpwnam`.
//!
//! These are deliberately not approximated. A `daemon` that exited 0 without
//! listening, or that served a wrong advertisement, would look like a success
//! to a harness comparing exit codes while corrupting whoever fetched from it.

use anyhow::{bail, Result};
use std::process::ExitCode;

/// The usage block `daemon` writes to stderr: 647 bytes, 12 lines.
const USAGE: &str = concat!(
    "usage: git daemon [--verbose] [--syslog] [--export-all]\n",
    "           [--timeout=<n>] [--init-timeout=<n>] [--max-connections=<n>]\n",
    "           [--strict-paths] [--base-path=<path>] [--base-path-relaxed]\n",
    "           [--user-path | --user-path=<path>]\n",
    "           [--interpolated-path=<path>]\n",
    "           [--reuseaddr] [--pid-file=<file>]\n",
    "           [--(enable|disable|allow-override|forbid-override)=<service>]\n",
    "           [--access-hook=<path>]\n",
    "           [--inetd | [--listen=<host_or_ipaddr>] [--port=<n>]\n",
    "                      [--detach] [--user=<user> [--group=<group>]]\n",
    "           [--log-destination=(stderr|syslog|none)]\n",
    "           [<directory>...]\n",
);

/// The service table `daemon` matches `--enable=`/`--disable=` and the two
/// `--*-override=` options against, in git's declaration order.
const SERVICES: [&str; 3] = ["upload-archive", "upload-pack", "receive-pack"];

/// Where `daemon` sends its log records, which also decides how it dies.
#[derive(Clone, Copy, PartialEq)]
enum LogDest {
    /// No `--syslog` or `--log-destination=` was given; resolved after parsing.
    Unset,
    Stderr,
    Syslog,
    None,
}

/// Everything the argument loop accumulates that a later check reads back.
struct Opts {
    inetd: bool,
    detach: bool,
    strict_paths: bool,
    /// `--port=<n>`, stored as git's `int listen_port` — 0 means "not given".
    listen_port: i32,
    /// One entry per `--listen=`, lowercased as git's `xstrdup_tolower()` does.
    listen_addrs: Vec<String>,
    log_dest: LogDest,
    user: Option<String>,
    group: Option<String>,
    base_path: Option<String>,
    /// Trailing `<directory>...`, i.e. git's `ok_paths`.
    ok_paths: usize,
}

/// `git daemon` — parse the command line and run every startup check, then bail
/// rather than bind a socket. See the module docs for what is and is not
/// covered.
pub fn daemon(args: &[String]) -> Result<ExitCode> {
    // Dispatch includes the verb at index 0. `daemon`'s positionals are
    // directory paths, and the loop stops at the first non-option argument, so
    // a leading literal verb must be stripped or it would be taken as the
    // directory list. Both spellings git installs are accepted.
    let args = match args.first().map(String::as_str) {
        Some("daemon" | "git-daemon") => &args[1..],
        _ => args,
    };

    let mut o = Opts {
        inetd: false,
        detach: false,
        strict_paths: false,
        listen_port: 0,
        listen_addrs: Vec::new(),
        log_dest: LogDest::Unset,
        user: None,
        group: None,
        base_path: None,
        ok_paths: 0,
    };

    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        i += 1;

        // Options taking a value are only ever matched in `--name=value` form;
        // the bare spelling falls through to the usage branch below.
        if let Some(v) = arg.strip_prefix("--listen=") {
            // `string_list_append(&listen_addr, xstrdup_tolower(v))`.
            o.listen_addrs.push(v.to_ascii_lowercase());
            continue;
        }
        if let Some(v) = arg.strip_prefix("--port=") {
            // git: `n = strtoul(v, &end, 0); if (*v && !*end)` — the value must
            // be non-empty and fully consumed, else fall through to usage.
            if let Some(n) = c_strtoul_full(v, 0) {
                o.listen_port = n as u32 as i32;
                continue;
            }
            return Ok(usage());
        }
        match arg {
            "--inetd" => {
                o.inetd = true;
                continue;
            }
            "--verbose" | "--reuseaddr" | "--base-path-relaxed" | "--export-all"
            | "--informative-errors" | "--no-informative-errors" | "--user-path"
            // `--serve` is git's undocumented per-connection child mode; it is
            // accepted by the parser and, unlike --inetd/--detach, does not
            // change the default log destination.
            | "--serve" => continue,
            "--syslog" => {
                o.log_dest = LogDest::Syslog;
                continue;
            }
            "--strict-paths" => {
                o.strict_paths = true;
                continue;
            }
            "--detach" => {
                o.detach = true;
                continue;
            }
            _ => {}
        }
        if let Some(v) = arg.strip_prefix("--log-destination=") {
            o.log_dest = match v {
                "stderr" => LogDest::Stderr,
                "syslog" => LogDest::Syslog,
                "none" => LogDest::None,
                _ => return Ok(die(&format!("unknown log destination '{v}'"))),
            };
            continue;
        }
        if let Some(v) = arg.strip_prefix("--timeout=") {
            match strtoul_ui(v) {
                Some(_) => continue,
                None => {
                    return Ok(die(&format!(
                        "invalid timeout '{v}', expecting a non-negative integer"
                    )))
                }
            }
        }
        if let Some(v) = arg.strip_prefix("--init-timeout=") {
            match strtoul_ui(v) {
                Some(_) => continue,
                None => {
                    return Ok(die(&format!(
                        "invalid init-timeout '{v}', expecting a non-negative integer"
                    )))
                }
            }
        }
        if let Some(v) = arg.strip_prefix("--max-connections=") {
            match strtol_i(v) {
                Some(_) => continue,
                None => {
                    return Ok(die(&format!(
                        "invalid max-connections '{v}', expecting an integer"
                    )))
                }
            }
        }
        // The four service switches share one lookup and one message.
        if let Some(name) = [
            "--enable=",
            "--disable=",
            "--allow-override=",
            "--forbid-override=",
        ]
        .iter()
        .find_map(|p| arg.strip_prefix(*p))
        {
            if !SERVICES.contains(&name) {
                return Ok(die(&format!("No such service {name}")));
            }
            continue;
        }
        if let Some(v) = arg.strip_prefix("--base-path=") {
            o.base_path = Some(v.to_string());
            continue;
        }
        if arg.starts_with("--interpolated-path=")
            || arg.starts_with("--access-hook=")
            || arg.starts_with("--pid-file=")
            || arg.starts_with("--user-path=")
        {
            continue;
        }
        if let Some(v) = arg.strip_prefix("--user=") {
            o.user = Some(v.to_string());
            continue;
        }
        if let Some(v) = arg.strip_prefix("--group=") {
            o.group = Some(v.to_string());
            continue;
        }
        if arg == "--" {
            // Everything after `--` is the directory list; `--` as the final
            // argument leaves it empty.
            o.ok_paths = args.len() - i;
            break;
        }
        if !arg.starts_with('-') {
            // The first non-option argument starts the directory list and ends
            // option parsing — later `-…` arguments are paths, not options.
            o.ok_paths = args.len() - (i - 1);
            break;
        }
        return Ok(usage());
    }

    // The default destination is syslog under --inetd or --detach, else stderr;
    // and only the syslog destination swaps in `daemon_die`, which logs and
    // exits 1 instead of writing `fatal:` and exiting 128.
    let log_dest = match o.log_dest {
        LogDest::Unset if o.inetd || o.detach => LogDest::Syslog,
        LogDest::Unset => LogDest::Stderr,
        d => d,
    };
    let quiet = log_dest == LogDest::Syslog;

    if o.inetd && (o.detach || o.group.is_some() || o.user.is_some()) {
        return Ok(die_maybe_quiet(
            "--detach, --user and --group are incompatible with --inetd",
            quiet,
        ));
    }
    if o.inetd && (o.listen_port != 0 || !o.listen_addrs.is_empty()) {
        return Ok(die_maybe_quiet(
            "--listen= and --port= are incompatible with --inetd",
            quiet,
        ));
    }
    if o.group.is_some() && o.user.is_none() {
        return Ok(die_maybe_quiet("--group supplied without --user", quiet));
    }
    if let Some(user) = &o.user {
        // git calls getpwnam(user) here, then getgrnam(group) if --group was
        // given, and dies "user not found - <u>" / "group not found - <g>".
        bail!(
            "--user={user:?} is not ported: dropping privileges is only meaningful for the \
             serving process this port does not have, so getpwnam(3)/getgrnam(3) are not called"
        );
    }
    if o.strict_paths && o.ok_paths == 0 {
        return Ok(die_maybe_quiet(
            "option --strict-paths requires '<directory>' arguments",
            quiet,
        ));
    }
    if let Some(base) = &o.base_path {
        if !std::path::Path::new(base).is_dir() {
            return Ok(die_maybe_quiet(
                &format!("base-path '{base}' does not exist or is not a directory"),
                quiet,
            ));
        }
    }

    // Past this point git either services one request from stdin (--inetd /
    // --serve) or enters the accept loop.
    if o.inetd {
        bail!(
            "serving a request over stdin (--inetd) is not ported: the vendored crates implement \
             only the client side of the git protocol (gix-transport/src/client, \
             gix-protocol handshake/fetch/ls_refs)"
        );
    }
    // `if (detach) { if (daemonize()) die(...) }` — before the sockets, so a
    // daemon that cannot fork never takes the port.
    if o.detach {
        bail!(
            "--detach is not ported: daemonising is a fork/setsid/redirect sequence with no \
             substrate in gitoxide, which is a repository-format library"
        );
    }

    // `serve()`: bind first, and die when nothing could be bound. This is the
    // last step that is observable without a client, and it is where
    // `git daemon` on a busy port ends up.
    let port = if o.listen_port == 0 {
        DEFAULT_GIT_PORT
    } else {
        o.listen_port
    };
    if let Some(code) = socksetup(&o.listen_addrs, port, log_dest, quiet) {
        return Ok(code);
    }

    bail!(
        "the git:// service loop is not ported: the listen sockets bind, but accepting a \
         connection needs a server-side upload-pack/receive-pack, and the vendored crates \
         implement only the client half (gix-transport/src/client, gix-protocol \
         handshake/fetch/ls_refs)"
    );
}

/// `daemon.c`'s `DEFAULT_GIT_PORT`.
const DEFAULT_GIT_PORT: i32 = 9418;

/// `socksetup()` followed by `serve()`'s `if (socklist.nr == 0) die(...)`.
///
/// Returns `Some(exit)` when not one socket could be bound — git's
/// `unable to allocate any listen sockets on port <n>` — and `None` when at
/// least one was, which is the point where git enters `service_loop()`.
///
/// Every socket opened here is closed again on return: there is no server-side
/// protocol implementation to hand it to (see the module docs), so holding the
/// port would only lock out a real daemon. What is reproduced is the part that
/// decides the exit status — which addresses are tried, in which order, and that
/// a failure on every one of them is fatal while a failure on some is not.
///
/// One deliberate divergence, not observable in the exit code: `IPV6_V6ONLY` is
/// not set (Rust's `TcpListener` has no pre-bind hook for it), so on a platform
/// that defaults it off the `::` bind also covers IPv4 and the following
/// `0.0.0.0` bind reports the port as taken — one socket where git gets two,
/// and a listen set that is not empty either way. `SO_REUSEADDR` needs no code:
/// `TcpListener::bind` sets it on every non-Windows target, as
/// `set_reuse_addr()` does.
fn socksetup(
    listen_addrs: &[String],
    port: i32,
    log_dest: LogDest,
    quiet: bool,
) -> Option<ExitCode> {
    let mut bound = 0usize;
    if listen_addrs.is_empty() {
        // `if (!listen_addr->nr) setup_named_sock(NULL, …)` — the wildcard.
        bound += setup_named_sock(None, port, log_dest);
    } else {
        for addr in listen_addrs {
            let socknum = setup_named_sock(Some(addr), port, log_dest);
            // `if (socknum == 0) logerror("unable to allocate any listen
            //  sockets for host %s on port %u", …)`.
            if socknum == 0 {
                logerror(
                    &format!(
                        "unable to allocate any listen sockets for host {addr} on port {}",
                        port as u32
                    ),
                    log_dest,
                );
            }
            bound += socknum;
        }
    }

    if bound == 0 {
        // `%u` of git's `int listen_port`. This `die` is the one that lands
        // *after* `freopen("/dev/null", "w", stderr)` at `cmd_main`'s line 1455,
        // so under the `none` destination the message is discarded while the
        // status stays 128 — unlike the startup checks above, which run before
        // the redirect.
        let msg = format!("unable to allocate any listen sockets on port {}", port as u32);
        if log_dest != LogDest::Stderr {
            return Some(ExitCode::from(if quiet { 1 } else { 128 }));
        }
        return Some(die_maybe_quiet(&msg, quiet));
    }
    None
}

/// `setup_named_sock()`: resolve one listen address and bind every result,
/// returning how many sockets came up. Neither a resolution failure nor a bind
/// failure is fatal on its own — only an empty socket list is, and that is the
/// caller's decision.
fn setup_named_sock(listen_addr: Option<&str>, port: i32, log_dest: LogDest) -> usize {
    // `getaddrinfo(listen_addr, pbuf, &hints, &ai0)` with `AI_PASSIVE` and
    // `AF_UNSPEC`. Rust has no passive resolution, so the wildcard is spelled
    // out as the two addresses the resolver returns for it, in that order; the
    // port goes in as text exactly as git's `xsnprintf(pbuf, "%d", …)` does, so
    // a value outside the port range fails resolution rather than being clamped.
    let hosts: Vec<&str> = match listen_addr {
        Some(addr) => vec![addr],
        None => vec!["::", "0.0.0.0"],
    };
    let mut addrs: Vec<std::net::SocketAddr> = Vec::new();
    let mut failure: Option<String> = None;
    for host in hosts {
        // A literal IPv6 address needs brackets before the port can be appended.
        let target = if host.contains(':') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        };
        match std::net::ToSocketAddrs::to_socket_addrs(&target) {
            Ok(found) => addrs.extend(found),
            Err(e) => {
                failure.get_or_insert_with(|| errno_text(&e));
            }
        }
    }

    if addrs.is_empty() {
        // `logerror("getaddrinfo() for %s failed: %s", listen_addr,
        //  gai_strerror(gai))` — C renders a NULL host as `(null)`.
        logerror(
            &format!(
                "getaddrinfo() for {} failed: {}",
                listen_addr.unwrap_or("(null)"),
                failure.unwrap_or_else(|| "no addresses returned".to_string())
            ),
            log_dest,
        );
        return 0;
    }

    let mut socknum = 0;
    for addr in addrs {
        match std::net::TcpListener::bind(addr) {
            Ok(listener) => {
                socknum += 1;
                // Closed immediately; see the note on `socksetup`.
                drop(listener);
            }
            // `logerror("Could not bind to %s: %s", ip2str(…), strerror(errno))`
            // — one line per address, and not fatal on its own.
            Err(e) => logerror(
                &format!("Could not bind to {}: {}", addr.ip(), errno_text(&e)),
                log_dest,
            ),
        }
    }
    socknum
}

/// `logerror()`: the message prefixed with the pid, on stderr — except under the
/// syslog and none destinations, where git has already pointed stderr at
/// /dev/null (`freopen("/dev/null", "w", stderr)`), so nothing is visible. The
/// syslog record itself is not reproduced; see the module docs.
fn logerror(msg: &str, log_dest: LogDest) {
    if log_dest == LogDest::Stderr {
        eprintln!("[{}] {msg}", std::process::id());
    }
}

/// `strerror(errno)`, which is what git appends: Rust adds its own
/// ` (os error <n>)` tail to the same text, so that tail is trimmed off.
fn errno_text(e: &std::io::Error) -> String {
    let text = e.to_string();
    match text.find(" (os error ") {
        Some(at) => text[..at].to_string(),
        None => text,
    }
}

/// git's `usage()`: the block on stderr, exit 129.
fn usage() -> ExitCode {
    eprint!("{USAGE}");
    ExitCode::from(129)
}

/// git's default `die()`: `fatal: <msg>` on stderr, exit 128.
fn die(msg: &str) -> ExitCode {
    eprintln!("fatal: {msg}");
    ExitCode::from(128)
}

/// A post-parse `die()`. Under the syslog destination `daemon_die` is installed,
/// which writes the record to syslog — not to stderr — and exits 1. The syslog
/// record is not reproduced; stdout, stderr and the exit code are.
fn die_maybe_quiet(msg: &str, quiet: bool) -> ExitCode {
    if quiet {
        return ExitCode::from(1);
    }
    die(msg)
}

/// C `strtoul(s, &end, base)`, returning the value, whether the conversion
/// overflowed, and the index `end` points at. `None` means no digits were
/// converted, i.e. C's `end == s`.
///
/// Handles the leading-whitespace skip, an optional sign, and base-0 prefix
/// detection (`0x` → 16, leading `0` → 8, else 10). Overflow saturates, as the
/// callers here only ever reject on it.
fn c_strtoul(s: &str, base: u32) -> Option<(u64, bool, bool, usize)> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    let mut negative = false;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        negative = b[i] == b'-';
        i += 1;
    }

    let mut base = base;
    // A `0x` prefix only counts when a hex digit follows it; otherwise the `0`
    // stands alone as the converted value and conversion stops at the `x`.
    let is_hex_prefix = |i: usize| {
        i + 2 < b.len() && b[i] == b'0' && b[i + 1] | 0x20 == b'x' && b[i + 2].is_ascii_hexdigit()
    };
    if base == 0 {
        if is_hex_prefix(i) {
            base = 16;
            i += 2;
        } else if i < b.len() && b[i] == b'0' {
            // The `0` is itself the first octal digit, so it is not consumed.
            base = 8;
        } else {
            base = 10;
        }
    } else if base == 16 && is_hex_prefix(i) {
        i += 2;
    }

    let digits_start = i;
    let mut value: u64 = 0;
    let mut overflow = false;
    while i < b.len() {
        let digit = match b[i] {
            c @ b'0'..=b'9' => u32::from(c - b'0'),
            c @ b'a'..=b'z' => u32::from(c - b'a') + 10,
            c @ b'A'..=b'Z' => u32::from(c - b'A') + 10,
            _ => break,
        };
        if digit >= base {
            break;
        }
        value = match value
            .checked_mul(u64::from(base))
            .and_then(|v| v.checked_add(u64::from(digit)))
        {
            Some(v) => v,
            None => {
                overflow = true;
                u64::MAX
            }
        };
        i += 1;
    }
    if i == digits_start {
        return None;
    }
    Some((value, negative, overflow, i))
}

/// `--port=`'s test: `strtoul(v, &end, 0)` accepted only when `v` is non-empty
/// and fully consumed. The result is truncated to `int` by the caller, so a
/// negative or overflowing value is still accepted — exactly as in C.
fn c_strtoul_full(v: &str, base: u32) -> Option<u64> {
    if v.is_empty() {
        return None;
    }
    let (value, negative, _, end) = c_strtoul(v, base)?;
    if end != v.len() {
        return None;
    }
    Some(if negative { value.wrapping_neg() } else { value })
}

/// git's `strtoul_ui(s, 10, &result)`: rejects a `-` anywhere in the string
/// before parsing, then requires full consumption, no overflow, and a value that
/// round-trips through `unsigned int`.
fn strtoul_ui(s: &str) -> Option<u32> {
    if s.contains('-') {
        return None;
    }
    let (value, _, overflow, end) = c_strtoul(s, 10)?;
    if overflow || end != s.len() {
        return None;
    }
    u32::try_from(value).ok()
}

/// git's `strtol_i(s, 10, &result)`: full consumption, no overflow, and a value
/// that round-trips through `int`. Unlike `strtoul_ui` it accepts negatives.
fn strtol_i(s: &str) -> Option<i32> {
    let (value, negative, overflow, end) = c_strtoul(s, 10)?;
    if overflow || end != s.len() {
        return None;
    }
    let signed = if negative {
        -i128::from(value)
    } else {
        i128::from(value)
    };
    i32::try_from(signed).ok()
}
