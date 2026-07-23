//! `git credential-cache` — the client half of the in-memory credential cache.
//!
//! Stock `credential-cache.c` is a thin relay: it turns the invocation into a
//! request for `git-credential-cache--daemon`, sends it over a Unix domain
//! socket, and copies whatever comes back straight to stdout. It never parses
//! the credential itself, so a faithful port needs no credential substrate at
//! all — only the wire framing, the socket-path rules, and git's option parser.
//!
//! Ported here, byte-for-byte against stock git (verified against `git version
//! 2.55.0`):
//!   * The request framing: `action=<op>\n`, `timeout=<n>\n`, then — for `get`,
//!     `store` and `erase` — stdin relayed verbatim to EOF. The write side is
//!     shut down, then every byte the daemon answers with is written to stdout
//!     unchanged (`capability[]=…`/`username=`/`password=` lines and all).
//!   * All four actions: `exit` (no stdin relay, never spawns a daemon), and
//!     `get`/`store`/`erase` (relay + spawn-on-demand). Any other action is
//!     silently ignored with exit 0, exactly as `cmd_main` does.
//!   * `--timeout <n>` (default 900) and `--socket <path>`, in the stuck
//!     (`--timeout=30`), separate (`--timeout 30`), negated (`--no-timeout`,
//!     which sets 0) and abbreviated (`--tim 5`) forms git's `parse_options`
//!     accepts, including `--`, and `-h` anywhere on the line.
//!   * `git_parse_int` on the timeout: leading whitespace, sign, base-0 digits
//!     (`0x10` → 16, `08` → invalid octal), and a `k`/`m`/`g` unit suffix
//!     (`1k` → 1024), with git's three distinct diagnostics for empty,
//!     malformed and out-of-`int`-range values.
//!   * The default socket path (`get_socket_path`): `~/.git-credential-cache/socket`
//!     when that directory exists, else `$XDG_CACHE_HOME/git/credential/socket`,
//!     else `~/.cache/git/credential/socket`.
//!   * Exit codes and streams: usage to stdout for `-h` and to stderr when no
//!     action is given (both 129), `error:` diagnostics to stderr (129), `fatal:`
//!     to stderr (128), and 0 for every path that reaches a socket — including a
//!     connect failure on `exit`, which stock git swallows silently.
//!
//! Not covered:
//!   * The daemon itself. `git-credential-cache--daemon` is its own command; this
//!     module only spawns it and requires its `ok\n` handshake. Spawning goes
//!     through `current_exe()`, which is what git's `git_cmd = 1` amounts to for
//!     the shadow binary, so the daemon that serves the cache is zvcs's own —
//!     when that subcommand is absent the handshake fails and this reports
//!     `fatal: cache daemon did not start: …` rather than pretending to cache.
//!   * `die_errno` texts come from Rust's `io::Error` with the trailing
//!     ` (os error N)` stripped, which matches `strerror` for the errnos these
//!     paths can produce but is not guaranteed to for exotic ones.
//!   * `git_config(git_default_config)`, which stock calls but whose settings
//!     nothing in this command reads.

use anyhow::Result;
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

/// `usage_with_options()` over `credential-cache`'s option table, verbatim.
const USAGE: &str = "usage: git credential-cache [<options>] <action>\n\
                     \n    \
                     --[no-]timeout <n>    number of seconds to cache credentials\n    \
                     --[no-]socket <path>  path of cache-daemon socket\n\n";

/// The long options git's `parse_options` knows here, in table order — the list
/// abbreviations are resolved against.
const OPTIONS: [&str; 2] = ["timeout", "socket"];

/// `git credential-cache` — see the module docs for exactly what is ported.
pub fn credential_cache(args: &[String]) -> Result<ExitCode> {
    let mut timeout: i32 = 900;
    let mut socket: Option<String> = None;
    let mut action: Option<&str> = None;
    let mut no_more_opts = false;

    let mut i = 1;
    while i < args.len() {
        let a = args[i].as_str();
        i += 1;

        if !no_more_opts && a == "--" {
            no_more_opts = true;
            continue;
        }
        if no_more_opts || !a.starts_with('-') || a == "-" {
            // git permutes, so options are still honoured after the action; only
            // the first non-option becomes `argv[0]`, the rest are ignored.
            if action.is_none() {
                action = Some(a);
            }
            continue;
        }

        if let Some(long) = a.strip_prefix("--") {
            let (name, inline) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (long, None),
            };
            let (name, negated) = match name.strip_prefix("no-") {
                Some(rest) => (rest, true),
                None => (name, false),
            };
            let Some(opt) = resolve_long(name) else {
                eprintln!("error: unknown option `{}'", &a[2..]);
                eprint!("{USAGE}");
                return Ok(ExitCode::from(129));
            };

            if negated {
                if inline.is_some() {
                    eprintln!("error: option `no-{opt}' takes no value");
                    return Ok(ExitCode::from(129));
                }
                // `OPT_INTEGER`'s unset form is 0; `OPT_STRING`'s is NULL, which
                // sends us back to the default socket path.
                match opt {
                    "timeout" => timeout = 0,
                    _ => socket = None,
                }
                continue;
            }

            let value = match inline {
                Some(v) => v,
                None => match args.get(i) {
                    Some(v) => {
                        i += 1;
                        v.as_str()
                    }
                    None => {
                        eprintln!("error: option `{opt}' requires a value");
                        return Ok(ExitCode::from(129));
                    }
                },
            };
            match opt {
                "timeout" => match parse_int(value) {
                    Ok(v) => timeout = v,
                    Err(msg) => {
                        eprintln!("error: {msg}");
                        return Ok(ExitCode::from(129));
                    }
                },
                _ => socket = Some(value.to_string()),
            }
            continue;
        }

        // Short options: git consumes the cluster left to right and neither `-h`
        // nor an unknown switch lets it continue, so only the first char matters
        // (`-hx` is help, `-xh` is the unknown switch).
        let c = a[1..].chars().next().expect("checked non-empty above");
        if c == 'h' {
            print!("{USAGE}");
        } else {
            eprintln!("error: unknown switch `{c}'");
            eprint!("{USAGE}");
        }
        return Ok(ExitCode::from(129));
    }

    let Some(action) = action else {
        eprint!("{USAGE}");
        return Ok(ExitCode::from(129));
    };

    let socket_path = match socket {
        Some(s) => PathBuf::from(s),
        None => match default_socket_path() {
            Some(p) => p,
            None => {
                eprintln!("fatal: unable to find a suitable socket path; use --socket");
                return Ok(ExitCode::from(128));
            }
        },
    };

    match action {
        // `exit` neither relays stdin nor starts a daemon: with nothing listening
        // there is nothing to shut down, and git returns 0 without a word.
        "exit" => do_cache(&socket_path, action, timeout, false, false),
        "get" | "store" | "erase" => do_cache(&socket_path, action, timeout, true, true),
        // "ignore unknown operation"
        _ => Ok(ExitCode::SUCCESS),
    }
}

/// Resolve a long-option name against the option table the way `parse_long_opt`
/// does: an exact match, otherwise an unambiguous prefix. The two options here
/// share no prefix, so the ambiguity diagnostic is unreachable.
fn resolve_long(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        return None;
    }
    if let Some(exact) = OPTIONS.iter().find(|o| **o == name) {
        return Some(exact);
    }
    let mut matches = OPTIONS.iter().filter(|o| o.starts_with(name));
    let first = matches.next()?;
    matches.next().is_none().then_some(*first)
}

/// `get_socket_path()`: the legacy directory wins when it exists, then XDG.
/// `None` only when `$HOME` is unset and `$XDG_CACHE_HOME` is unset or empty,
/// which is git's `die("unable to find a suitable socket path")`.
fn default_socket_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME");
    if let Some(home) = &home {
        let old_dir = Path::new(home).join(".git-credential-cache");
        if old_dir.is_dir() {
            return Some(old_dir.join("socket"));
        }
    }
    // `xdg_cache_home("credential/socket")`.
    if let Some(cache_home) = std::env::var_os("XDG_CACHE_HOME") {
        if !cache_home.is_empty() {
            return Some(Path::new(&cache_home).join("git/credential/socket"));
        }
    }
    home.map(|h| Path::new(&h).join(".cache/git/credential/socket"))
}

/// `do_cache()`: build the request, try it, and — for the relaying actions —
/// start a daemon and try once more if nothing was listening.
fn do_cache(
    socket: &Path,
    action: &str,
    timeout: i32,
    spawn: bool,
    relay: bool,
) -> Result<ExitCode> {
    let mut request = format!("action={action}\ntimeout={timeout}\n").into_bytes();
    if relay {
        // `strbuf_read(&buf, 0, 0)` — stdin verbatim, to EOF, appended as-is.
        if let Err(e) = std::io::stdin().lock().read_to_end(&mut request) {
            return fatal(&format!("unable to relay credential: {}", errno(&e)));
        }
    }

    match send_request(socket, &request) {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(Some(msg)) => fatal(&msg),
        Err(None) => {
            if !spawn {
                return Ok(ExitCode::SUCCESS);
            }
            if let Err(msg) = spawn_daemon(socket) {
                return fatal(&msg);
            }
            match send_request(socket, &request) {
                Ok(()) => Ok(ExitCode::SUCCESS),
                Err(Some(msg)) => fatal(&msg),
                Err(None) => fatal("unable to connect to cache daemon: Connection refused"),
            }
        }
    }
}

/// `send_request()`: write the request, half-close, copy the answer to stdout.
///
/// `Err(None)` is git's `-1` return — the socket was not connectable, which is
/// not an error by itself; `Err(Some(msg))` is a `die_errno` after connecting.
fn send_request(socket: &Path, request: &[u8]) -> Result<(), Option<String>> {
    let mut stream = UnixStream::connect(socket).map_err(|_| None)?;

    stream
        .write_all(request)
        .map_err(|e| Some(format!("unable to write to cache daemon: {}", errno(&e))))?;
    // `shutdown(fd, SHUT_WR)` — unchecked in git, and a failure here surfaces on
    // the read side anyway.
    let _ = stream.shutdown(Shutdown::Write);

    let mut stdout = std::io::stdout().lock();
    let mut buf = [0u8; 1024];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => stdout
                .write_all(&buf[..n])
                .map_err(|e| Some(format!("write error: {}", errno(&e))))?,
            // git treats a reset exactly like a clean EOF here.
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => break,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                return Err(Some(format!("read error from cache daemon: {}", errno(&e))))
            }
        }
    }
    stdout
        .flush()
        .map_err(|e| Some(format!("write error: {}", errno(&e))))
}

/// `spawn_daemon()`: start the daemon detached (git never waits for it) and
/// require its `ok\n` handshake, read from a pipe capped at git's 128 bytes.
/// The daemon's stderr stays inherited so its own `fatal:` reaches the terminal.
fn spawn_daemon(socket: &Path) -> Result<(), String> {
    // `daemon.git_cmd = 1` — run our own git, not whatever is first on PATH.
    let exe = std::env::current_exe()
        .map_err(|e| format!("unable to start cache daemon: {}", errno(&e)))?;
    let mut child = Command::new(exe)
        .arg("credential-cache--daemon")
        .arg(socket)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("unable to start cache daemon: {}", errno(&e)))?;

    let pipe = child.stdout.take().expect("stdout piped");
    let mut buf = Vec::new();
    pipe.take(128)
        .read_to_end(&mut buf)
        .map_err(|e| format!("unable to read result code from cache daemon: {}", errno(&e)))?;
    if buf != b"ok\n" {
        return Err(format!(
            "cache daemon did not start: {}",
            String::from_utf8_lossy(&buf)
        ));
    }
    Ok(())
}

/// `git_parse_int()` as `OPT_INTEGER` applies it, with git's three diagnostics.
fn parse_int(arg: &str) -> Result<i32, String> {
    if arg.is_empty() {
        return Err("option `timeout' expects a numerical value".to_string());
    }
    let range = || {
        format!("value {arg} for option `timeout' not in range [-2147483648,2147483647]")
    };
    let malformed =
        || "option `timeout' expects an integer value with an optional k/m/g suffix".to_string();

    let Some((value, rest)) = strtoimax(arg) else {
        return Err(malformed());
    };
    // `get_unit_factor()`: nothing, or one k/m/g, case-insensitively.
    let factor: i128 = match rest {
        "" => 1,
        "k" | "K" => 1024,
        "m" | "M" => 1024 * 1024,
        "g" | "G" => 1024 * 1024 * 1024,
        _ => return Err(malformed()),
    };
    let scaled = value.checked_mul(factor).ok_or_else(range)?;
    i32::try_from(scaled).map_err(|_| range())
}

/// `strtoimax(value, &end, 0)`: optional leading whitespace and sign, then a
/// base-0 integer — `0x`/`0X` hex, a leading `0` octal, otherwise decimal.
/// Returns the value and the unconsumed tail; `None` when no digit was consumed
/// (git's `end == value` check) or the digits overflow even `intmax_t`.
fn strtoimax(arg: &str) -> Option<(i128, &str)> {
    let body = arg.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let (negative, body) = match body.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, body.strip_prefix('+').unwrap_or(body)),
    };

    let (radix, digits, prefix_len) = if let Some(hex) = body
        .strip_prefix("0x")
        .or_else(|| body.strip_prefix("0X"))
    {
        (16, hex, 2)
    } else if body.len() > 1 && body.starts_with('0') {
        (8, &body[1..], 1)
    } else {
        (10, body, 0)
    };

    let end = digits
        .find(|c: char| !c.is_digit(radix))
        .unwrap_or(digits.len());
    if end == 0 {
        // No digit followed the prefix: C stops after the leading `0` it did
        // consume, so `08` is 0 with tail `8` and `0x` is 0 with tail `x`.
        return match prefix_len {
            0 => None,
            _ => Some((0, &body[1..])),
        };
    }

    let magnitude = i128::from_str_radix(&digits[..end], radix).ok()?;
    let value = if negative { -magnitude } else { magnitude };
    Some((value, &digits[end..]))
}

/// `die()`: the message on stderr with git's prefix, exit 128.
fn fatal(msg: &str) -> Result<ExitCode> {
    eprintln!("fatal: {msg}");
    Ok(ExitCode::from(128))
}

/// Render an `io::Error` the way `die_errno`'s `strerror` would, i.e. without
/// Rust's trailing ` (os error N)`.
fn errno(e: &std::io::Error) -> String {
    let text = e.to_string();
    match text.find(" (os error ") {
        Some(at) => text[..at].to_string(),
        None => text,
    }
}
