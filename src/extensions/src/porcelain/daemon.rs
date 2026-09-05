//! `git daemon` — the `git://` protocol server.
//! **`--inetd` — one request off stdin, answered — is ported end to end. The
//! listening accept loop is not.**
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
//!   * `--inetd`, up to the service call: `execute()` reads the request packet
//!     off stdin, `run_service()` applies every check ahead of the service —
//!     the `--enable`/`--disable`/`--allow-override`/`--forbid-override` table,
//!     `path_ok()` (`daemon_avoid_alias()`, `--user-path`, `--base-path`,
//!     `--base-path-relaxed`, `enter_repo()`, `--strict-paths` and the trailing
//!     `<directory>` list), the `git-daemon-export-ok` test that `--export-all`
//!     waives, and the per-repository `daemon.uploadpack`/`daemon.uploadarch`/
//!     `daemon.receivepack` override — and `daemon_error()` writes the one `ERR`
//!     packet git writes, with `--informative-errors` deciding whether it names
//!     the real reason. Every refusal exits 255, as `cmd_main` returning
//!     `execute()`'s `-1` does. A request that survives all of it is ANSWERED:
//!     `run_service_command()` runs the service as a `git` child over the
//!     client's own stdin and stdout with its stderr drained into the log, and
//!     the daemon exits with the child's status. `parse_extra_args()` turns the
//!     blocks behind the request's NUL into the child's `GIT_PROTOCOL`, so a v2
//!     client gets the v2 capability advertisement and a v0 one the ref
//!     advertisement.
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
//!   1. **The accept loop.** `poll()` over the listen sockets, forking a child
//!      per connection, `--max-connections` child reaping, `--timeout`/
//!      `--init-timeout` alarm handling, `--detach` daemonisation and
//!      `--pid-file` are process-level work with no substrate in gitoxide,
//!      which is a repository-format library. `--inetd` reaches the same
//!      `execute()` without any of it, which is why the request path is ported
//!      and the listening one is not.
//!   2. **`--interpolated-path=` and `--access-hook=`.** `%CH` and `%IP` expand
//!      to the canonical hostname and IP address of the accepted connection,
//!      and the hook is handed the same; neither exists without a connection to
//!      resolve, so a request that would use one bails.
//!   3. **`--user=`/`--group=` privilege drop.** `getpwnam(3)`/`getgrnam(3)`
//!      are not called: they are POSIX identity lookups that only the listening
//!      daemon performs, and `--inetd` refuses `--user` before reaching them.
//!      The lookup cannot be faked from `/etc/passwd` either —
//!      Darwin resolves users through Directory Services, so a file scan would
//!      report `user not found` for users that exist. `--group` without
//!      `--user` needs no lookup and is checked faithfully; a present `--user`
//!      bails at exactly the point git would call `getpwnam`.
//!
//! These are deliberately not approximated. A `daemon` that exited 0 without
//! listening would look like a success to a harness comparing exit codes while
//! corrupting whoever fetched from it.

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
    /// `--base-path-relaxed`: retry `enter_repo()` without the base path.
    base_path_relaxed: bool,
    /// `--export-all`, git's `export_all_trees`.
    export_all: bool,
    /// `--informative-errors` / `--no-informative-errors`, git's
    /// `informative_errors`: whether a refusal names its real reason or the
    /// single generic one.
    informative_errors: bool,
    /// `--user-path` (empty) or `--user-path=<path>`; `None` means `~` requests
    /// are refused outright.
    user_path: Option<String>,
    /// `--interpolated-path=<format>`.
    interpolated_path: Option<String>,
    /// `--access-hook=<path>`.
    access_hook: Option<String>,
    /// `enabled` and `overridable` per entry of [`SERVICES`], in that order.
    enabled: [bool; 3],
    overridable: [bool; 3],
    /// `--timeout=<n>`, git's `static unsigned int timeout`. Handed on to
    /// `upload-pack` as `--timeout=%u` (daemon.c:485), and 0 unless given.
    timeout: u32,
    /// Trailing `<directory>...`, i.e. git's `ok_paths`.
    ok_paths: Vec<String>,
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
        base_path_relaxed: false,
        export_all: false,
        informative_errors: false,
        user_path: None,
        interpolated_path: None,
        access_hook: None,
        // `daemon_service[]` (daemon.c:499): only `upload-pack` is on by
        // default, and all three may be turned on and off per repository.
        enabled: [false, true, false],
        overridable: [true, true, true],
        timeout: 0,
        ok_paths: Vec::new(),
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
            "--verbose" | "--reuseaddr"
            // `--serve` is git's undocumented per-connection child mode; it is
            // accepted by the parser and, unlike --inetd/--detach, does not
            // change the default log destination.
            | "--serve" => continue,
            "--base-path-relaxed" => {
                o.base_path_relaxed = true;
                continue;
            }
            "--export-all" => {
                o.export_all = true;
                continue;
            }
            "--informative-errors" => {
                o.informative_errors = true;
                continue;
            }
            "--no-informative-errors" => {
                o.informative_errors = false;
                continue;
            }
            // `else if (!strcmp(arg, "--user-path")) { user_path = ""; }`
            "--user-path" => {
                o.user_path = Some(String::new());
                continue;
            }
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
                Some(n) => {
                    o.timeout = n;
                    continue;
                }
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
        if let Some((prefix, name)) = [
            "--enable=",
            "--disable=",
            "--allow-override=",
            "--forbid-override=",
        ]
        .iter()
        .find_map(|p| arg.strip_prefix(*p).map(|name| (*p, name)))
        {
            let Some(index) = SERVICES.iter().position(|s| *s == name) else {
                return Ok(die(&format!("No such service {name}")));
            };
            // `enable_service()` / `make_service_overridable()` (daemon.c:505),
            // which set the flag on the named entry and return.
            match prefix {
                "--enable=" => o.enabled[index] = true,
                "--disable=" => o.enabled[index] = false,
                "--allow-override=" => o.overridable[index] = true,
                _ => o.overridable[index] = false,
            }
            continue;
        }
        if let Some(v) = arg.strip_prefix("--base-path=") {
            o.base_path = Some(v.to_string());
            continue;
        }
        if let Some(v) = arg.strip_prefix("--interpolated-path=") {
            o.interpolated_path = Some(v.to_string());
            continue;
        }
        if let Some(v) = arg.strip_prefix("--access-hook=") {
            o.access_hook = Some(v.to_string());
            continue;
        }
        if let Some(v) = arg.strip_prefix("--user-path=") {
            o.user_path = Some(v.to_string());
            continue;
        }
        if arg.starts_with("--pid-file=") {
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
            o.ok_paths = args[i..].to_vec();
            break;
        }
        if !arg.starts_with('-') {
            // The first non-option argument starts the directory list and ends
            // option parsing — later `-…` arguments are paths, not options.
            o.ok_paths = args[i - 1..].to_vec();
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
    if o.strict_paths && o.ok_paths.is_empty() {
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
        return execute(&o);
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

// ---------------------------------------------------------------------------
// `--inetd`: one request, read from stdin (daemon.c's `execute()`).
// ---------------------------------------------------------------------------

/// The config key each entry of [`SERVICES`] reads for its per-repository
/// override, in the same order — `daemon_service[]`'s `config_name` column
/// (daemon.c:499).
const SERVICE_CONFIG_NAMES: [&str; 3] = ["uploadarch", "uploadpack", "receivepack"];

/// `execute()` (daemon.c:747): read one request off stdin and answer it.
///
/// ```c
/// pktlen = packet_read(0, packet_buffer, sizeof(packet_buffer), 0);
/// len = strlen(line);
/// if (len && line[len-1] == '\n')
///         line[len-1] = 0;
/// …
/// if (skip_prefix(line, "git-", &arg) &&
///     skip_prefix(arg, s->name, &arg) &&
///     *arg++ == ' ')
///         return run_service(arg, s, &hi, &env);
/// …
/// logerror("Protocol error: '%s'", line);
/// return -1;
/// ```
///
/// `cmd_main` returns that `-1` straight out of `main`, so every refusal here
/// exits 255. Under `--inetd` the log destination defaults to syslog and
/// `cmd_main` has already redirected stderr to `/dev/null`, so nothing this
/// function logs is visible — only the `ERR` packet on stdout and the status.
fn execute(o: &Opts) -> Result<ExitCode> {
    let (line, pktlen) = match read_request_packet() {
        Some(request) => request,
        // A read error is git's `packet_read()` dying; there is no request to
        // answer either way.
        None => return Ok(ExitCode::from(255)),
    };

    // `len = strlen(line)`: the request line ends at the first NUL, and the
    // extra arguments live behind it. git overwrites a trailing newline with a
    // NUL but leaves `len` alone, so the extras still start at `line + len + 1`.
    let len = line.iter().position(|b| *b == 0).unwrap_or(line.len());
    let mut request = &line[..len];
    if request.last() == Some(&b'\n') {
        request = &request[..request.len() - 1];
    }
    let saw_extended_args = len != pktlen;
    let request = String::from_utf8_lossy(request).into_owned();

    // ```c
    // if (len != pktlen)
    //         parse_extra_args(&hi, &env, line + len + 1, pktlen - len - 1);
    // ```
    //
    // (daemon.c:772-773.) The extras behind the request's NUL become the
    // `GIT_PROTOCOL` value the service is run with, which is how a v2 client
    // negotiates over `git://` — without it `upload-pack` answers a v2 request
    // with the v0 advertisement.
    let git_protocol = if saw_extended_args {
        match parse_extra_args(&line[len + 1..pktlen], o) {
            Ok(v) => v,
            Err(code) => return Ok(code),
        }
    } else {
        None
    };

    for (index, name) in SERVICES.iter().enumerate() {
        let Some(rest) = request
            .strip_prefix("git-")
            .and_then(|a| a.strip_prefix(*name))
        else {
            continue;
        };
        let Some(dir) = rest.strip_prefix(' ') else {
            continue;
        };
        return run_service(o, index, dir, saw_extended_args, git_protocol.as_deref());
    }

    // `logerror("Protocol error: '%s'", line)` and `return -1`: no packet is
    // written, and under `--inetd` the log line goes to syslog.
    Ok(ExitCode::from(255))
}

/// One pkt-line off stdin, as `packet_read(0, …, 0)` reads it.
///
/// Returns the payload and its length. A flush packet reads back as an empty
/// payload, which is what makes a client that connected and left a protocol
/// error rather than a request.
fn read_request_packet() -> Option<(Vec<u8>, usize)> {
    use std::io::Read;

    let mut header = [0u8; 4];
    let mut stdin = std::io::stdin().lock();
    stdin.read_exact(&mut header).ok()?;
    let size = usize::from_str_radix(std::str::from_utf8(&header).ok()?, 16).ok()?;
    if size == 0 {
        return Some((Vec::new(), 0));
    }
    if size < 4 {
        return None;
    }
    let mut payload = vec![0u8; size - 4];
    stdin.read_exact(&mut payload).ok()?;
    Some((payload.clone(), payload.len()))
}

/// `parse_extra_args()` (daemon.c:638) over the bytes behind the request line's
/// NUL, returning the `GIT_PROTOCOL` value it builds.
///
/// ```c
/// extra_args = parse_host_arg(hi, extra_args, buflen);
/// for (; extra_args < end; extra_args += strlen(extra_args) + 1) {
///         const char *arg = extra_args;
///         if (*arg) {
///                 if (git_protocol.len > 0)
///                         strbuf_addch(&git_protocol, ':');
///                 strbuf_addstr(&git_protocol, arg);
///         }
/// }
/// ```
///
/// The first NUL-terminated block is the `host=` attribute, consumed by
/// `parse_host_arg()` (`:605`) and never forwarded; anything else in that first
/// position is `die("Invalid request")`. Every block after it joins the value
/// with `:`, so `\0version=2\0` — the block a v2 client sends behind an empty
/// second NUL — arrives as `GIT_PROTOCOL=version=2`.
///
/// The hostname itself is only logged and used by `--interpolated-path=`, which
/// is not ported, so it is parsed for its length and dropped.
fn parse_extra_args(extra: &[u8], o: &Opts) -> std::result::Result<Option<String>, ExitCode> {
    /// One NUL-terminated block, and what is left after it. A block with no NUL
    /// runs to the end, as C's `strlen` over a buffer git NUL-terminates does.
    fn split_block(buf: &[u8]) -> (&[u8], &[u8]) {
        match buf.iter().position(|b| *b == 0) {
            Some(at) => (&buf[..at], &buf[at + 1..]),
            None => (buf, &buf[buf.len()..]),
        }
    }

    let mut rest = extra;
    // `if (extra_args < end && *extra_args)` — an empty first block is not a
    // host attribute and is left for the loop below.
    if rest.first().is_some_and(|b| *b != 0) {
        if rest.len() >= 5 && rest[..5].eq_ignore_ascii_case(b"host=") {
            rest = split_block(rest).1;
        }
        // `if (extra_args < end && *extra_args) die("Invalid request");` — a
        // first block that is not `host=`, or a second block crowding straight
        // up against it, is refused before any service runs.
        if rest.first().is_some_and(|b| *b != 0) {
            return Err(die_maybe_quiet(
                "Invalid request",
                o.log_dest == LogDest::Syslog,
            ));
        }
    }

    let mut protocol = String::new();
    while !rest.is_empty() {
        let (block, after) = split_block(rest);
        rest = after;
        if block.is_empty() {
            continue;
        }
        if !protocol.is_empty() {
            protocol.push(':');
        }
        protocol.push_str(&String::from_utf8_lossy(block));
    }
    Ok((!protocol.is_empty()).then_some(protocol))
}

/// `run_service()` (daemon.c:367), up to the point where the service itself
/// would run.
///
/// Every refusal below is `daemon_error()`, which writes one `ERR` packet and
/// returns -1:
///
/// ```c
/// static int daemon_error(const char *dir, const char *msg)
/// {
///         if (!informative_errors)
///                 msg = "access denied or repository not exported";
///         packet_write_fmt(1, "ERR %s: %s", msg, dir);
///         return -1;
/// }
/// ```
fn run_service(
    o: &Opts,
    index: usize,
    dir: &str,
    saw_extended_args: bool,
    git_protocol: Option<&str>,
) -> Result<ExitCode> {
    let mut enabled = o.enabled[index];
    let overridable = o.overridable[index];

    // `if (!enabled && !service->overridable)` — a service turned off by name
    // that no repository may turn back on.
    if !enabled && !overridable {
        return Ok(daemon_error(o, dir, "service not enabled"));
    }

    let Some(_path) = path_ok(o, dir, saw_extended_args)? else {
        return Ok(daemon_error(o, dir, "no such repository"));
    };

    // `path_ok()` left the process inside the repository, so this is git's
    // `access("git-daemon-export-ok", F_OK)` verbatim.
    if !o.export_all && !std::path::Path::new("git-daemon-export-ok").exists() {
        return Ok(daemon_error(o, dir, "repository not exported"));
    }

    if overridable {
        // `repo_config_get_bool(the_repository, "daemon.<config_name>", &enabled)`
        // leaves `enabled` alone when the key is unset, which is how a service
        // that is off by default stays off.
        let repo = gix::open_opts(".", gix::open::Options::default().open_path_as_is(true))?;
        if let Some(value) = repo
            .config_snapshot()
            .boolean(&format!("daemon.{}", SERVICE_CONFIG_NAMES[index]))
        {
            enabled = value;
        }
    }
    if !enabled {
        return Ok(daemon_error(o, dir, "service not enabled"));
    }

    if let Some(hook) = &o.access_hook {
        bail!(
            "--access-hook={hook:?} is not ported: it runs per accepted request, and the request \
             itself is only answered as far as the refusals above (see the module docs)"
        );
    }

    // `return service->fn(env);` (daemon.c:441). Each of the three is one
    // `run_service_command()` call with the service's own flags
    // (daemon.c:481-510):
    //
    // ```c
    // static int upload_pack(const struct strvec *env)
    // {
    //         struct child_process cld = CHILD_PROCESS_INIT;
    //         strvec_pushl(&cld.args, "upload-pack", "--strict", NULL);
    //         strvec_pushf(&cld.args, "--timeout=%u", timeout);
    //         strvec_pushv(&cld.env, env->v);
    //         return run_service_command(&cld);
    // }
    // ```
    //
    // `upload_archive()` and `receive_pack()` are the same with no flags.
    let timeout = format!("--timeout={}", o.timeout);
    let args: &[&str] = match SERVICES[index] {
        "upload-pack" => &["upload-pack", "--strict", &timeout],
        "upload-archive" => &["upload-archive"],
        _ => &["receive-pack"],
    };
    run_service_command(args, git_protocol, o.log_dest)
}

/// ```c
/// static int run_service_command(struct child_process *cld)
/// {
///         strvec_push(&cld->args, ".");
///         cld->git_cmd = 1;
///         cld->err = -1;
///         if (start_command(cld))
///                 return -1;
///         close(0);
///         close(1);
///         copy_to_log(cld->err);
///         return finish_command(cld);
/// }
/// ```
///
/// (daemon.c:465-479.) The service is a `git` CHILD, not an in-process call:
/// `path_ok()` has already left the process inside the repository, so the
/// directory argument is `.`; stdin and stdout are the client's and are
/// inherited untouched; the child's stderr is a pipe drained into the daemon's
/// log by `copy_to_log()` (`:444`), which is why a served request prints nothing
/// on the daemon's own stderr under `--inetd` — git has pointed that at
/// `/dev/null` (`:1454`) and only `--log-destination=stderr` brings it back.
///
/// The exit status is the child's, which `cmd_main` returns as the daemon's own
/// (`:1459`): `upload-pack` and `receive-pack` answering a client that hangs up
/// after the advertisement exit 128, and a v2 `upload-pack` that reaches the end
/// of its command loop exits 0.
fn run_service_command(
    args: &[&str],
    git_protocol: Option<&str>,
    log_dest: LogDest,
) -> Result<ExitCode> {
    use std::io::BufRead;

    let mut cmd = std::process::Command::new(crate::hosted::git_exe()?);
    cmd.args(args)
        .arg(".")
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::piped());
    // `strvec_pushv(&cld.env, env->v)` — the only variable `parse_extra_args()`
    // ever builds, and the one a v2 client's `version=2` block turns into.
    if let Some(protocol) = git_protocol {
        cmd.env("GIT_PROTOCOL", protocol);
    }
    let mut child = cmd.spawn()?;
    if let Some(err) = child.stderr.take() {
        for line in std::io::BufReader::new(err).lines().map_while(Result::ok) {
            logerror(&line, log_dest);
        }
    }
    let status = child.wait()?;
    // `finish_command()` reports a signalled child as `128 + signal`
    // (run-command.c's `wait_or_whine`), which is the status git returns.
    let code = match status.code() {
        Some(code) => code,
        None => {
            use std::os::unix::process::ExitStatusExt;
            128 + status.signal().unwrap_or(0)
        }
    };
    Ok(ExitCode::from(code as u8))
}

/// `daemon_error()` (daemon.c:301): one `ERR` pkt-line on stdout, exit 255.
fn daemon_error(o: &Opts, dir: &str, msg: &str) -> ExitCode {
    use std::io::Write;

    let msg = if o.informative_errors {
        msg
    } else {
        "access denied or repository not exported"
    };
    let payload = format!("ERR {msg}: {dir}");
    let mut out = std::io::stdout().lock();
    let _ = write!(out, "{:04x}{payload}", payload.len() + 4);
    let _ = out.flush();
    ExitCode::from(255)
}

/// `path_ok()` (daemon.c:147): turn the requested virtual path into a real
/// repository, entering it, or `None` when it must be refused.
///
/// `enter_repo()` chdirs into whatever it settles on, which is what makes the
/// `git-daemon-export-ok` test and the `daemon.*` config read in
/// [`run_service`] relative lookups.
fn path_ok(o: &Opts, directory: &str, saw_extended_args: bool) -> Result<Option<String>> {
    // `if (daemon_avoid_alias(dir)) return NULL` — the request must be absolute
    // or a `~` path, and may not contain `//`, `/./` or `/../`.
    if daemon_avoid_alias(directory) {
        return Ok(None);
    }

    let mut dir = directory.to_string();
    if directory.starts_with('~') {
        let Some(user_path) = &o.user_path else {
            // `logerror("'%s': User-path not allowed", dir)`.
            return Ok(None);
        };
        if !user_path.is_empty() {
            // `snprintf(rpath, …, "%.*s/%s%.*s", namlen, dir, user_path, restlen, slash)`.
            let namlen = directory.find('/').unwrap_or(directory.len());
            dir = format!(
                "{}/{}{}",
                &directory[..namlen],
                user_path,
                &directory[namlen..]
            );
        }
    } else if o.interpolated_path.is_some() && saw_extended_args {
        bail!(
            "--interpolated-path= is not ported: %CH and %IP expand to the canonical hostname and \
             IP address of the accepted connection, which this port has no connection to resolve"
        );
    } else if let Some(base) = &o.base_path {
        // `if (*dir != '/') return NULL` — only absolute virtual paths may be
        // prefixed with the base path.
        if !dir.starts_with('/') {
            return Ok(None);
        }
        dir = format!("{base}{dir}");
    }

    let mut path = enter_repo(&dir, o.strict_paths);
    if path.is_none() && o.base_path.is_some() && o.base_path_relaxed {
        // "if we fail and base_path_relaxed is enabled, try without prefixing
        // the base path".
        path = enter_repo(directory, o.strict_paths);
    }
    let Some(path) = path else {
        return Ok(None);
    };

    // The `ok_paths` gate: a request must live under one of the trailing
    // `<directory>` arguments, and without `--strict-paths` a repository below
    // one of them counts too. With no directory list at all, only
    // `--strict-paths` denies.
    if !o.ok_paths.is_empty() {
        for ok in &o.ok_paths {
            if path.starts_with(ok.as_str())
                && (path.len() == ok.len()
                    || (!o.strict_paths && path.as_bytes().get(ok.len()) == Some(&b'/')))
            {
                return Ok(Some(path));
            }
        }
    } else if !o.strict_paths {
        return Ok(Some(path));
    }

    // `logerror("'%s': not in directory list", path)` — deny by default.
    Ok(None)
}

/// `enter_repo()` (setup.c:1817), reduced to what the daemon asks of it: settle
/// on one of git's four suffixes, chdir into it, and confirm what we landed in
/// is a git directory.
///
/// ```c
/// static const char *suffix[] = { "/.git", "", ".git/.git", ".git", NULL };
/// ```
///
/// Trailing slashes are trimmed off the request first (all but a leading one),
/// and the *validated* path git returns is the request plus the suffix that
/// matched — not the directory it entered, which is why a gitfile still reports
/// the name the client asked for.
///
/// The `~` expansion `enter_repo()` performs is [`super::upload_pack`]'s: `~/`
/// against `$HOME`, and `~user` refused rather than passed through, because a
/// passwd lookup is not available in the vendored crates.
fn enter_repo(path: &str, strict: bool) -> Option<String> {
    let candidates: Vec<(String, String)> = if strict {
        vec![(path.to_string(), path.to_string())]
    } else {
        let bytes = path.as_bytes();
        let mut len = path.len();
        while len > 1 && bytes[len - 1] == b'/' {
            len -= 1;
        }
        let base = &path[..len];
        let used_base = match base.strip_prefix('~') {
            None => base.to_string(),
            Some(rest) => {
                if !rest.is_empty() && !rest.starts_with('/') {
                    // `interpolate_path()` would consult the passwd database.
                    return None;
                }
                match std::env::var_os("HOME") {
                    Some(home) => format!("{}{rest}", home.to_string_lossy()),
                    None => base.to_string(),
                }
            }
        };
        ["/.git", "", ".git/.git", ".git"]
            .iter()
            .map(|suffix| (format!("{used_base}{suffix}"), format!("{base}{suffix}")))
            .collect()
    };

    let options = gix::open::Options::default().open_path_as_is(true);
    for (used, validated) in candidates {
        if gix::open_opts(&used, options.clone()).is_err() {
            continue;
        }
        // `if (chdir(used_path.buf)) return NULL;` then `is_git_directory(".")`.
        if std::env::set_current_dir(&used).is_err() {
            return None;
        }
        return Some(validated);
    }
    None
}

/// `daemon_avoid_alias()` (path.c:1331): refuse a request that is neither
/// absolute nor `~`-rooted, and any that contains `//`, `/./`, `/../`, `/.` or
/// `/..` at the end.
///
/// "sl becomes true immediately after seeing '/' and continues to be true as
/// long as dots continue after that without intervening non-dot character."
fn daemon_avoid_alias(path: &str) -> bool {
    let bytes = path.as_bytes();
    if !matches!(bytes.first(), Some(b'/') | Some(b'~')) {
        return true;
    }
    let (mut sl, mut ndot) = (true, 0usize);
    for index in 1..=bytes.len() {
        // The C reads the NUL terminator as the final character.
        let ch = bytes.get(index).copied().unwrap_or(0);
        if sl {
            match ch {
                b'.' => ndot += 1,
                b'/' => {
                    if ndot < 3 {
                        // reject //, /./ and /../
                        return true;
                    }
                    ndot = 0;
                }
                0 => {
                    // reject /.$ and /..$
                    return 0 < ndot && ndot < 3;
                }
                _ => {
                    sl = false;
                    ndot = 0;
                }
            }
        } else if ch == 0 {
            return false;
        } else if ch == b'/' {
            sl = true;
            ndot = 0;
        }
    }
    false
}
