//! `git fsmonitor--daemon` — the *client* half of git's built-in filesystem
//! monitor (`builtin/fsmonitor--daemon.c`).
//!
//! Covered (byte-identical to stock git): the `status` and `stop` subcommands,
//! including their exit codes; the option grammar (`--[no-]detach`,
//! `--[no-]ipc-threads <n>`, `--[no-]start-timeout <n>`) with git's
//! unknown-option / missing-value / non-numeric-value diagnostics; the
//! argc-not-one usage text (129); the `invalid 'ipc-threads' value` and
//! `bare repository ... is incompatible with fsmonitor` fatals (128); and the
//! `Unhandled subcommand` fatal (128).
//!
//! Not covered (rejected, never faked): `start` and `run`. Those require the
//! daemon half — a platform filesystem-notification backend (FSEvents on macOS,
//! inotify on Linux, `ReadDirectoryChangesW` on Windows) plus git's simple-IPC
//! *server* with its cookie/token change-list machinery. Neither exists in the
//! vendored gitoxide crates. Also not covered: the network-filesystem socket
//! path fallback (`fsmonitor.socketDir`, `$HOME/.git-fsmonitor-*`), which is
//! rejected rather than guessed, and non-unix platforms.

use anyhow::{bail, Result};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Exact `usage_with_options()` block emitted by `builtin/fsmonitor--daemon.c`.
const USAGE: &str = "\
usage: git fsmonitor--daemon start [<options>]
   or: git fsmonitor--daemon run [<options>]
   or: git fsmonitor--daemon stop
   or: git fsmonitor--daemon status

    --[no-]detach         detach from console
    --[no-]ipc-threads <n>
                          use <n> ipc worker threads
    --[no-]start-timeout <n>
                          max seconds to wait for background daemon startup
";

/// Name of the simple-IPC socket git creates inside the (per-worktree) git dir.
const IPC_SOCKET: &str = "fsmonitor--daemon.ipc";

/// `git fsmonitor--daemon` — query or stop the built-in filesystem monitor.
///
/// Only the client subcommands are ported:
///   * `status` — prints `fsmonitor-daemon is watching '<worktree>'` and exits 0
///     when a daemon is listening on `<gitdir>/fsmonitor--daemon.ipc`, otherwise
///     `fsmonitor-daemon is not watching '<worktree>'` and exits 1. Both lines go
///     to stdout, as in `do_as_client__status()`.
///   * `stop` — sends the simple-IPC `quit` command (a pkt-line payload followed
///     by a flush packet, matching `ipc_client_send_command()`), then polls until
///     the socket stops accepting connections and exits 0 with no output. When no
///     daemon is listening git's `fsmonitor_ipc__send_command()` dies, so this
///     prints `fatal: fsmonitor--daemon is not running` and exits 128.
///
/// `start` and `run` are rejected: the daemon side needs a filesystem-event
/// backend and a simple-IPC server, neither of which is available here.
pub fn fsmonitor__daemon(args: &[String]) -> Result<ExitCode> {
    // `args[0]` is the subcommand itself when dispatched; tolerate its absence.
    let rest = match args.first() {
        Some(a) if a == "fsmonitor--daemon" => &args[1..],
        _ => args,
    };

    // git runs setup before parse_options (RUN_SETUP), so repository discovery
    // failures win over option errors.
    let repo = gix::discover(".")?;

    let mut ipc_threads: i64 = 8; // fsmonitor__ipc_threads default
    let mut positional: Vec<&str> = Vec::new();
    let mut no_more_options = false;

    let mut i = 0;
    while i < rest.len() {
        let a = rest[i].as_str();
        if no_more_options || !a.starts_with('-') || a == "-" {
            positional.push(a);
            i += 1;
            continue;
        }
        match a {
            "--" => no_more_options = true,
            "--detach" | "--no-detach" => {}
            "--no-ipc-threads" => ipc_threads = 0,
            "--start-timeout" | "--no-start-timeout" => {
                if a == "--start-timeout" {
                    if rest.get(i + 1).is_none() {
                        return Ok(option_requires_value("start-timeout"));
                    }
                    i += 1;
                }
            }
            "--ipc-threads" => {
                let Some(value) = rest.get(i + 1) else {
                    return Ok(option_requires_value("ipc-threads"));
                };
                match parse_magnitude(value) {
                    Some(n) => ipc_threads = n,
                    None => return Ok(option_expects_integer("ipc-threads")),
                }
                i += 1;
            }
            s if s.starts_with("--ipc-threads=") => {
                match parse_magnitude(&s["--ipc-threads=".len()..]) {
                    Some(n) => ipc_threads = n,
                    None => return Ok(option_expects_integer("ipc-threads")),
                }
            }
            s if s.starts_with("--start-timeout=") => {}
            s if s.starts_with("--") => {
                eprint!("error: unknown option `{}'\n{USAGE}\n", &s[2..]);
                return Ok(ExitCode::from(129));
            }
            s => {
                // Short forms; git reports the first unknown switch character.
                let c = s.chars().nth(1).unwrap_or('-');
                eprint!("error: unknown switch `{c}'\n{USAGE}\n");
                return Ok(ExitCode::from(129));
            }
        }
        i += 1;
    }

    // `if (argc != 1) usage_with_options(...)`.
    if positional.len() != 1 {
        eprint!("{USAGE}\n");
        return Ok(ExitCode::from(129));
    }
    if ipc_threads < 1 {
        eprintln!("fatal: invalid 'ipc-threads' value ({ipc_threads})");
        return Ok(ExitCode::from(128));
    }

    // `die(_("bare repository '%s' is incompatible with fsmonitor"), xgetcwd())`
    // — the message carries the *current directory*, not the git dir.
    let Some(workdir) = repo.workdir() else {
        let cwd = std::env::current_dir()?;
        eprintln!(
            "fatal: bare repository '{}' is incompatible with fsmonitor",
            real_path(&cwd).display()
        );
        return Ok(ExitCode::from(128));
    };
    let worktree = real_path(workdir);

    // The socket normally lives in the per-worktree git dir. git relocates it
    // when that directory is on a network filesystem or `fsmonitor.socketDir`
    // is set; that path derivation is not replicated, so reject it rather than
    // report a wrong answer against the default location.
    if repo.config_snapshot().string("fsmonitor.socketDir").is_some() {
        bail!("`fsmonitor.socketDir` socket relocation is not supported");
    }
    let socket = repo.git_dir().join(IPC_SOCKET);

    match positional[0] {
        "status" => {
            if is_listening(&socket) {
                println!("fsmonitor-daemon is watching '{}'", worktree.display());
                Ok(ExitCode::SUCCESS)
            } else {
                println!("fsmonitor-daemon is not watching '{}'", worktree.display());
                Ok(ExitCode::from(1))
            }
        }
        "stop" => stop(&socket),
        "start" | "run" => bail!(
            "`{}` needs the daemon substrate (a filesystem-notification backend and a simple-IPC server), which the vendored gitoxide crates do not provide (ported: status, stop)",
            positional[0]
        ),
        other => {
            eprintln!("fatal: Unhandled subcommand '{other}'");
            Ok(ExitCode::from(128))
        }
    }
}

/// `do_as_client__send_stop()`: send `quit`, then wait for the daemon to exit.
fn stop(socket: &Path) -> Result<ExitCode> {
    let Some(mut stream) = connect(socket) else {
        eprintln!("fatal: fsmonitor--daemon is not running");
        return Ok(ExitCode::from(128));
    };

    // simple-IPC request framing: one pkt-line holding the command, then a
    // flush packet. The `quit` command answers with a bare flush packet.
    {
        use std::io::{Read, Write};
        let payload = "quit";
        write!(stream, "{:04x}{payload}0000", payload.len() + 4)?;
        stream.flush()?;
        let mut answer = Vec::new();
        stream.read_to_end(&mut answer)?;
    }
    drop(stream);

    // `while (fsmonitor_ipc__get_state() == IPC_STATE__LISTENING) sleep_millisec(50);`
    while is_listening(socket) {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    Ok(ExitCode::SUCCESS)
}

/// True when a daemon accepts connections on `socket`, mirroring
/// `ipc_client_try_connect()`'s `IPC_STATE__LISTENING` verdict.
fn is_listening(socket: &Path) -> bool {
    connect(socket).is_some()
}

#[cfg(unix)]
fn connect(socket: &Path) -> Option<std::os::unix::net::UnixStream> {
    std::os::unix::net::UnixStream::connect(socket).ok()
}

#[cfg(not(unix))]
fn connect(_socket: &Path) -> Option<std::net::TcpStream> {
    // git speaks named pipes on Windows; that transport is not implemented.
    None
}

/// git realpath()s the worktree and the cwd before printing them.
fn real_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// `OPT_INTEGER` accepts an optional `k`/`m`/`g` suffix.
fn parse_magnitude(value: &str) -> Option<i64> {
    let (digits, scale) = match value.chars().last() {
        Some('k') | Some('K') => (&value[..value.len() - 1], 1024),
        Some('m') | Some('M') => (&value[..value.len() - 1], 1024 * 1024),
        Some('g') | Some('G') => (&value[..value.len() - 1], 1024 * 1024 * 1024),
        _ => (value, 1),
    };
    digits.parse::<i64>().ok()?.checked_mul(scale)
}

fn option_requires_value(name: &str) -> ExitCode {
    eprint!("error: option `{name}' requires a value\n{USAGE}\n");
    ExitCode::from(129)
}

fn option_expects_integer(name: &str) -> ExitCode {
    eprint!(
        "error: option `{name}' expects an integer value with an optional k/m/g suffix\n{USAGE}\n"
    );
    ExitCode::from(129)
}
