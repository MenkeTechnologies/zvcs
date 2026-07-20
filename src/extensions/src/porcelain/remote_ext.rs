//! `git remote-ext` — bridge a smart transport to an external command.
//!
//! This is a remote helper, not a repository command: git invokes it as
//! `git remote-ext <remote> <url>` for every `ext::<command>` URL, then speaks
//! the `gitremote-helpers(7)` line protocol to it on stdin/stdout. It touches no
//! object database and no ref store, so nothing here goes through gitoxide — the
//! whole command is argument expansion plus process plumbing, and it is ported
//! from `builtin/remote-ext.c` directly.
//!
//! Covered, byte-for-byte against git 2.55.0 on Darwin:
//!   * `-h` / `--help-all` as the *only* argument → the usage line on **stdout**,
//!     exit 129.
//!   * any argument count other than 3 (`remote-ext`, `<remote>`, `<url>`) → the
//!     same usage line on **stderr**, exit 129.
//!   * the command loop: lines are read with git's 4094-byte `fgets` cap and have
//!     *all* trailing whitespace stripped (`isspace`, `\v` included), then
//!       - `capabilities` → `*connect\n\n` on stdout, loop again;
//!       - `connect <service>` → `\n` on stdout, then run the child;
//!       - anything else → `Bad command` on stderr (no newline), exit 1;
//!       - EOF → exit 0.
//!   * `<url>` expansion, ported from `strip_escapes()`: `% ` → literal space,
//!     `%%` → `%`, `%s` → the service without its `git-` prefix, `%S` → the full
//!     service name; `%G<repo>` and `%V<vhost>` are consumed as the leading two
//!     bytes of an argument (only there) and are not passed to the child.
//!   * `%G` sends a `git://`-style service request as the first thing on the
//!     child's stdin: a pkt-line holding `<service> SP <repo> NUL`, plus
//!     `host=<vhost> NUL` when `%V` was given.
//!   * `GIT_EXT_SERVICE` / `GIT_EXT_SERVICE_NOPREFIX` in the child's environment.
//!   * the bidirectional copy loop: our stdin → child stdin (closed on EOF) and
//!     child stdout → our stdout, concurrently, with the child's stderr inherited.
//!   * exit codes: the child's own status; 128 + signal when it dies of one;
//!     128 for `fatal: Bad remote-ext placeholder '%<c>'.`, for
//!     `fatal: remote-ext command has incomplete placeholder`, and for
//!     `fatal: Can't run specified command`; 134 when the expansion leaves no
//!     command at all.
//!
//! Three deliberate divergences, none reachable from a real caller:
//!   * git reads the command loop through stdio, so bytes that arrive in the same
//!     `read(2)` as the `connect` line are swallowed by the `FILE` buffer and
//!     never reach the child. This port carries them over to the child instead.
//!     Emulating the loss would mean emulating one libc's buffer sizing; and
//!     `transport-helper` waits for the `\n` acknowledgement before sending
//!     protocol data, so nothing is ever queued behind that line in practice.
//!   * an empty expansion aborts git via `BUG()`, i.e. `SIGABRT`, which a shell
//!     reports as 134. This port exits *normally* with 134 — same `$?`, but
//!     `WIFEXITED` rather than `WIFSIGNALED`. The `BUG:` text quotes git 2.55.0's
//!     source location and will drift with git's line numbering.
//!   * `GIT_TRANSLOOP_DEBUG` (per-read/per-write tracing of the copy loop on
//!     stderr) is not implemented.

use anyhow::Result;
use std::io::{Read, Write};
use std::process::{Command, ExitCode, ExitStatus, Stdio};

/// The usage string, verbatim from `builtin/remote-ext.c`.
const USAGE_MSG: &str = "usage: git remote-ext <remote> <url>\n";

/// `MAXCOMMAND` in `builtin/remote-ext.c` is 4096 and the loop calls
/// `fgets(buffer, MAXCOMMAND - 1, stdin)`, which stores at most 4094 bytes plus
/// the terminating NUL. A longer line is therefore split into several commands.
const MAX_COMMAND_BYTES: usize = 4094;

/// `git remote-ext` — the `ext::` remote helper.
///
/// See the module docs for the covered surface and the three divergences.
/// `args[0]` is the subcommand, so `args.len()` is C's `argc`, which this command
/// checks exactly (git rejects anything but 3).
pub fn remote_ext(args: &[String]) -> Result<ExitCode> {
    // `show_usage_if_asked()`: only when the help flag is the sole argument.
    // git 2.55.0 puts this on stdout but still leaves 129 in `$?`.
    if args.len() == 2 && (args[1] == "-h" || args[1] == "--help-all") {
        print!("{USAGE_MSG}");
        return Ok(ExitCode::from(129));
    }
    if args.len() != 3 {
        eprint!("{USAGE_MSG}");
        return Ok(ExitCode::from(129));
    }

    command_loop(&args[2])
}

/// Port of `command_loop()`: serve helper commands until `connect` or EOF.
fn command_loop(child_cmd: &str) -> Result<ExitCode> {
    loop {
        let Some(mut line) = read_command_line()? else {
            // `fgets` returning NULL without `ferror` is a clean EOF: exit(0).
            return Ok(ExitCode::SUCCESS);
        };

        // "Strip end of line characters": C's `isspace`, so `\v` counts too.
        while line.last().is_some_and(|b| is_c_space(*b)) {
            line.pop();
        }

        if line.as_slice() == b"capabilities".as_slice() {
            print!("*connect\n\n");
            std::io::stdout().flush()?;
        } else if let Some(service) = line.strip_prefix(b"connect ") {
            print!("\n");
            std::io::stdout().flush()?;
            // The service name is always ASCII on the wire; anything else could
            // not name a program git would run, so a lossy read is faithful.
            let service = String::from_utf8_lossy(service).into_owned();
            return run_child(child_cmd, &service);
        } else {
            // git omits the newline here.
            eprint!("Bad command");
            return Ok(ExitCode::FAILURE);
        }
    }
}

/// One `fgets` worth of stdin: up to and including `\n`, capped at git's 4094
/// bytes. `None` marks EOF with nothing read.
///
/// Reading a byte at a time is what keeps the *rest* of stdin intact for the
/// child: `std::io::Stdin` buffers globally, so whatever this over-reads stays in
/// the same buffer the copy loop drains later.
fn read_command_line() -> Result<Option<Vec<u8>>> {
    let mut stdin = std::io::stdin();
    let mut line = Vec::new();
    let mut byte = [0u8; 1];

    while line.len() < MAX_COMMAND_BYTES {
        if stdin.read(&mut byte)? == 0 {
            break;
        }
        line.push(byte[0]);
        if byte[0] == b'\n' {
            break;
        }
    }

    Ok(if line.is_empty() { None } else { Some(line) })
}

/// C's `isspace()` in the C locale.
fn is_c_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// Port of `run_child()`: expand the URL, spawn the command, and pump both
/// directions until they close.
fn run_child(arg: &str, service: &str) -> Result<ExitCode> {
    let service_noprefix = service.strip_prefix("git-").unwrap_or(service);

    let mut argv: Vec<Vec<u8>> = Vec::new();
    let mut git_req: Option<Vec<u8>> = None;
    let mut git_req_vhost: Option<Vec<u8>> = None;

    // `parse_argv()`: `strip_escapes()` consumes one argument per call and
    // advances `arg` past the separating space.
    let mut rest = arg.as_bytes();
    while !rest.is_empty() {
        let expanded = match strip_escapes(
            rest,
            service,
            service_noprefix,
            &mut git_req,
            &mut git_req_vhost,
        ) {
            Ok((expanded, next)) => {
                rest = next;
                expanded
            }
            // `die()`: `fatal: <msg>` on stderr, exit 128.
            Err(msg) => {
                eprintln!("fatal: {msg}");
                return Ok(ExitCode::from(128));
            }
        };
        if let Some(expanded) = expanded {
            argv.push(expanded);
        }
    }

    if argv.is_empty() {
        // `start_command()` trips a `BUG()`, which aborts; a shell reports 134.
        eprintln!("BUG: run-command.c:413: command is empty");
        return Ok(ExitCode::from(134));
    }

    let program = os_str(&argv[0]);
    let mut command = Command::new(&program);
    for a in &argv[1..] {
        command.arg(os_str(a));
    }
    // `strip_escapes()` sets these with `setenv()` before the fork; scoping them
    // to the child is the same thing from the child's point of view.
    command
        .env("GIT_EXT_SERVICE", service)
        .env("GIT_EXT_SERVICE_NOPREFIX", service_noprefix)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // `child.err = 0`: the child shares our stderr.
        .stderr(Stdio::inherit());

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            eprintln!(
                "error: cannot run {}: {}",
                String::from_utf8_lossy(&argv[0]),
                strerror(&e)
            );
            eprintln!("fatal: Can't run specified command");
            return Ok(ExitCode::from(128));
        }
    };

    let mut child_in = child.stdin.take().expect("stdin was piped");
    let mut child_out = child.stdout.take().expect("stdout was piped");

    // `%G` turns the connection into an in-line `git://` service request.
    if let Some(repo) = &git_req {
        if let Err(e) = send_git_request(&mut child_in, service, repo, git_req_vhost.as_deref()) {
            eprintln!("fatal: {e}");
            return Ok(ExitCode::from(128));
        }
    }

    // `bidirectional_transfer_loop()`: the git-to-program half runs alongside the
    // program-to-git half, and the child's stdin is closed once ours hits EOF.
    let git_to_program = std::thread::spawn(move || -> std::io::Result<()> {
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 8192];
        loop {
            let n = stdin.read(&mut buf)?;
            if n == 0 {
                break;
            }
            child_in.write_all(&buf[..n])?;
            child_in.flush()?;
        }
        drop(child_in);
        Ok(())
    });

    let program_to_git = {
        let stdout = std::io::stdout();
        let mut stdout = stdout.lock();
        let r = std::io::copy(&mut child_out, &mut stdout).and_then(|_| stdout.flush());
        r.err()
    };

    let git_to_program = git_to_program.join().ok().and_then(|r| r.err());

    // git reports each half's failure and keeps going to reap the child.
    if let Some(e) = &program_to_git {
        eprintln!("error: write(stdout) failed: {}", strerror(e));
    }
    if let Some(e) = &git_to_program {
        eprintln!("error: write(remote output) failed: {}", strerror(e));
    }

    let status = child.wait()?;

    // `if (!r) r = finish_command(&child);` — a copy failure wins over the
    // child's status, and git's loop reports failure as 1.
    if program_to_git.is_some() || git_to_program.is_some() {
        return Ok(ExitCode::FAILURE);
    }
    Ok(ExitCode::from(finish_code(&status, &argv[0])))
}

/// Port of `strip_escapes()`: expand one argument of the `ext::` URL.
///
/// Returns the expanded argument (`None` when it was a `%G`/`%V` directive, which
/// is captured into `git_req`/`git_req_vhost` instead) together with the rest of
/// the URL, already advanced past the separating space. `Err` carries the exact
/// text git would pass to `die()`.
fn strip_escapes<'a>(
    s: &'a [u8],
    service: &str,
    service_noprefix: &str,
    git_req: &mut Option<Vec<u8>>,
    git_req_vhost: &mut Option<Vec<u8>>,
) -> std::result::Result<(Option<Vec<u8>>, &'a [u8]), String> {
    // Pass 1: find where the argument ends, validating every placeholder. `%G`
    // and `%V` are legal only as the argument's first two bytes.
    let mut rpos = 0usize;
    let mut escape = false;
    let mut special = 0u8;

    while rpos < s.len() && (escape || s[rpos] != b' ') {
        if escape {
            match s[rpos] {
                b' ' | b'%' | b's' | b'S' => {}
                c @ (b'G' | b'V') => {
                    special = c;
                    if rpos != 1 {
                        return Err(format!("Bad remote-ext placeholder '%{}'.", c as char));
                    }
                }
                c => return Err(format!("Bad remote-ext placeholder '%{}'.", c as char)),
            }
            escape = false;
        } else {
            escape = s[rpos] == b'%';
        }
        rpos += 1;
    }
    if escape && rpos >= s.len() {
        return Err("remote-ext command has incomplete placeholder".to_string());
    }

    let mut next = &s[rpos..];
    if next.first() == Some(&b' ') {
        next = &next[1..];
    }

    // Pass 2: substitute. A `%G`/`%V` argument skips its own two-byte marker.
    let mut ret: Vec<u8> = Vec::new();
    let mut rpos = if special != 0 { 2 } else { 0 };
    let mut escape = false;

    while rpos < s.len() && (escape || s[rpos] != b' ') {
        if escape {
            match s[rpos] {
                c @ (b' ' | b'%') => ret.push(c),
                b's' => ret.extend_from_slice(service_noprefix.as_bytes()),
                b'S' => ret.extend_from_slice(service.as_bytes()),
                // Unreachable: pass 1 rejected every other escape.
                _ => {}
            }
            escape = false;
        } else if s[rpos] == b'%' {
            escape = true;
        } else {
            ret.push(s[rpos]);
        }
        rpos += 1;
    }

    match special {
        b'G' => {
            *git_req = Some(ret);
            Ok((None, next))
        }
        b'V' => {
            *git_req_vhost = Some(ret);
            Ok((None, next))
        }
        _ => Ok((Some(ret), next)),
    }
}

/// Port of `send_git_request()`: one pkt-line carrying the `git://` service
/// request, i.e. `<service> SP <repo> NUL` plus an optional `host=<vhost> NUL`.
fn send_git_request(
    out: &mut impl Write,
    service: &str,
    repo: &[u8],
    vhost: Option<&[u8]>,
) -> std::io::Result<()> {
    let mut data: Vec<u8> = Vec::new();
    data.extend_from_slice(service.as_bytes());
    data.push(b' ');
    data.extend_from_slice(repo);
    data.push(0);
    if let Some(vhost) = vhost {
        data.extend_from_slice(b"host=");
        data.extend_from_slice(vhost);
        data.push(0);
    }

    // pkt-line: a 4-digit lowercase hex length that counts its own four bytes.
    write!(out, "{:04x}", data.len() + 4)?;
    out.write_all(&data)?;
    out.flush()
}

/// Port of `wait_or_whine()`'s reporting for `finish_command()`: the child's exit
/// status, or 128 + signal when it was killed (announced unless it was `SIGINT`
/// or `SIGQUIT`, which git treats as the user's own doing).
fn finish_code(status: &ExitStatus, argv0: &[u8]) -> u8 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            if signal != libc_sigint() && signal != libc_sigquit() {
                eprintln!(
                    "error: {} died of signal {signal}",
                    String::from_utf8_lossy(argv0)
                );
            }
            return (signal + 128) as u8;
        }
    }
    #[cfg(not(unix))]
    let _ = argv0;
    status.code().unwrap_or(1) as u8
}

#[cfg(unix)]
fn libc_sigint() -> i32 {
    2
}

#[cfg(unix)]
fn libc_sigquit() -> i32 {
    3
}

/// The bare `strerror()` text, without Rust's ` (os error N)` suffix, so the
/// diagnostics read exactly like git's.
fn strerror(e: &std::io::Error) -> String {
    let text = e.to_string();
    match text.find(" (os error ") {
        Some(at) => text[..at].to_string(),
        None => text,
    }
}

/// An argument as the OS sees it. The `ext::` URL is arbitrary bytes and must
/// reach the child unmangled, so bypass `String` on platforms that allow it.
#[cfg(unix)]
fn os_str(bytes: &[u8]) -> std::ffi::OsString {
    use std::os::unix::ffi::OsStringExt;
    std::ffi::OsString::from_vec(bytes.to_vec())
}

#[cfg(not(unix))]
fn os_str(bytes: &[u8]) -> std::ffi::OsString {
    std::ffi::OsString::from(String::from_utf8_lossy(bytes).into_owned())
}
