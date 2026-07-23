//! `git shell` — restricted login shell for Git-only SSH access.
//!
//! Stock `git-shell` is a C builtin (`shell.c`) whose whole job is argument
//! validation and dispatch: it is a *front end*, not a repository operation, so
//! it ports onto gitoxide without needing any object-database substrate. What it
//! dispatches *to* is the substrate question, and that is handled by delegating
//! to the sibling ports.
//!
//! Covered, byte-verified against git 2.55.0 on Darwin:
//!   * **Argument shapes.** No arguments → interactive mode. `-c <command>` →
//!     one-shot mode. The historical single-argument `"cvs server"` form, which
//!     git rewrites into the `-c` path. Anything else → `fatal: Run with no
//!     arguments or with -c cmd`, exit 128 (this includes `-h`, which stock git
//!     does not special-case here).
//!   * **The `git foo` → `git-foo` rewrite** (`shell.c`: only when byte 3 is a
//!     whitespace character).
//!   * **The server-command table** — `git-receive-pack`, `git-upload-pack`,
//!     `git-upload-archive`. As of 2.55.0 the `cvs` entry is gone from the table
//!     (verified: `git shell -c "cvs server"` reports `unrecognized command`),
//!     so it is absent here too. The single argument is `sq_dequote`d exactly as
//!     git does; a missing argument, a non-single-quoted argument, or one
//!     starting with `-` gives `fatal: bad argument`, exit 128. Dispatch then
//!     goes in-process to [`super::receive_pack`] / [`super::upload_pack`] /
//!     [`super::upload_archive`] — which is where the protocol-level coverage
//!     limits live; see those modules' own headers.
//!   * **`cd_to_homedir()`** before the custom-command path (and before
//!     interactive mode, but *after* the server-command table — the ordering is
//!     git's and is observable): `fatal: could not determine user's home
//!     directory; HOME is unset` / `fatal: could not chdir to user's home
//!     directory`, exit 128.
//!   * **`split_cmdline()`** ported from git's `alias.c`, including the quirk
//!     that `argv[0]` always exists (so `-c ""` and `-c "   "` both reach the
//!     exec attempt and fail with `unrecognized command '<raw>'`), and the
//!     `unclosed quote` diagnostic: `fatal: invalid command format '<raw>':
//!     unclosed quote`, exit 128.
//!   * **`is_valid_cmd_name()`** (no `.` and no `/` anywhere) and
//!     `make_cmd()` (`git-shell-commands/<name>`). In `-c` mode an invalid name
//!     or any exec failure both yield `fatal: unrecognized command '<argv[2]>'`
//!     — quoting the *original*, un-rewritten string — exit 128.
//!   * **Interactive mode**: the `~/git-shell-commands` readable+executable
//!     gate (`fatal: Interactive git shell is not enabled.` plus the `hint:`
//!     line, exit 128), `no-interactive-login` short-circuit (its exit status
//!     becomes ours; exec failure → 127), the silent `help` invocation, the
//!     `git> ` prompt on stderr, `quit`/`logout`/`exit`/`bye`, the empty-line
//!     no-op, `unrecognized command '<prog>'` on ENOENT, `invalid command
//!     format '<line>'` for a name with `.`/`/`, and EOF printing a newline to
//!     stderr and exiting 0.
//!
//! Two deliberate deviations, both unobservable in normal use:
//!   * git `execv`s the custom command, replacing the process; this spawns and
//!     waits, propagating the exit code. A custom command killed by a signal
//!     therefore makes us exit `128 + signal` rather than dying by that signal.
//!   * git gates interactive mode with `access(dir, R_OK | X_OK)`; without a
//!     `libc` dependency that is approximated by `read_dir()` (read) plus
//!     `metadata(dir/".")` (search), which agrees on both Linux and Darwin for
//!     directories but would diverge for a non-directory at that path.
//!
//! Not covered: `git shell --help`, which the `git` wrapper turns into a man
//! page rather than ever reaching `shell.c`. It bails rather than guessing.

use anyhow::{bail, Result};
use std::ffi::OsStr;
use std::io::{BufRead, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// `shell.c`'s `COMMAND_DIR`, resolved relative to the user's home directory.
const COMMAND_DIR: &str = "git-shell-commands";
/// `shell.c`'s `HELP_COMMAND`.
const HELP_COMMAND: &str = "git-shell-commands/help";
/// `shell.c`'s `NOLOGIN_COMMAND`.
const NOLOGIN_COMMAND: &str = "git-shell-commands/no-interactive-login";

/// The server-side commands `git shell` will run, in `shell.c`'s `cmd_list`
/// order. Every entry takes the `do_generic_cmd` path.
const CMD_LIST: [&str; 3] = ["git-receive-pack", "git-upload-pack", "git-upload-archive"];

/// `git shell` — restricted login shell for Git-only SSH access.
///
/// `args` excludes the program name, so it maps onto `shell.c`'s `argv[1..]`.
pub fn shell(args: &[String]) -> Result<ExitCode> {
    if args.iter().any(|a| a == "--help") {
        bail!("`git shell --help` renders the man page, which is not ported");
    }

    // `argc == 2 && argv[1] == "cvs server"`: git shifts argv so the string is
    // treated as if it followed `-c`. With `cvs` gone from `cmd_list` this now
    // just routes to the custom-command path, exactly as it does upstream.
    let original: &str = if args.len() == 1 && args[0] == "cvs server" {
        &args[0]
    } else if args.is_empty() {
        return interactive();
    } else if args.len() == 2 && args[0] == "-c" {
        &args[1]
    } else {
        // We do not accept any other mode, since it may leak information.
        return die("Run with no arguments or with -c cmd");
    };

    // Accept "git foo" as if the caller said "git-foo".
    let mut prog = original.as_bytes().to_vec();
    if prog.len() > 3 && prog.starts_with(b"git") && is_space(prog[3]) {
        prog[3] = b'-';
    }

    for name in CMD_LIST {
        let n = name.as_bytes();
        if !prog.starts_with(n) {
            continue;
        }
        let arg = match prog.get(n.len()) {
            None => None,
            Some(b' ') => Some(&prog[n.len() + 1..]),
            Some(_) => continue,
        };
        return do_generic_cmd(name, arg);
    }

    if let Some(code) = cd_to_homedir() {
        return Ok(code);
    }

    let Some(argv) = split_cmdline(&prog) else {
        return die(&format!("invalid command format '{original}': unclosed quote"));
    };

    if is_valid_cmd_name(&argv[0]) {
        let path = make_cmd(&argv[0]);
        let mut cmd = Command::new(path);
        for a in &argv[1..] {
            cmd.arg(OsStr::from_bytes(a));
        }
        if let Ok(status) = cmd.status() {
            return Ok(exit_code_of(status));
        }
    }
    die(&format!("unrecognized command '{original}'"))
}

/// `shell.c: do_generic_cmd()` — validate the single quoted argument and hand
/// off to the named server command.
fn do_generic_cmd(me: &str, arg: Option<&[u8]>) -> Result<ExitCode> {
    let Some(arg) = arg.and_then(sq_dequote) else {
        return die("bad argument");
    };
    if arg.first() == Some(&b'-') {
        return die("bad argument");
    }
    let Ok(arg) = String::from_utf8(arg) else {
        bail!("non-UTF-8 repository argument is not supported")
    };

    let argv = [arg];
    match &me["git-".len()..] {
        "receive-pack" => super::receive_pack(&argv),
        "upload-pack" => super::upload_pack(&argv),
        "upload-archive" => super::upload_archive(&argv),
        other => bail!("unsupported server command {other:?}"),
    }
}

/// `shell.c: main()`'s `argc == 1` branch plus `run_shell()`.
fn interactive() -> Result<ExitCode> {
    if let Some(code) = cd_to_homedir() {
        return Ok(code);
    }
    if !readable_and_searchable(Path::new(COMMAND_DIR)) {
        eprintln!(
            "fatal: Interactive git shell is not enabled.\n\
             hint: ~/{COMMAND_DIR} should exist and have read and execute access."
        );
        return Ok(ExitCode::from(128));
    }
    run_shell()
}

/// `shell.c: run_shell()` — the `git> ` prompt loop.
fn run_shell() -> Result<ExitCode> {
    // Interactive login disabled: run the hook and take its status.
    if Path::new(NOLOGIN_COMMAND).exists() {
        return Ok(match Command::new(NOLOGIN_COMMAND).status() {
            Ok(status) => exit_code_of(status),
            Err(_) => ExitCode::from(127),
        });
    }

    // Print help if enabled; failure to exec it is silent.
    let _ = Command::new(HELP_COMMAND).status();

    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut line: Vec<u8> = Vec::new();
    loop {
        eprint!("git> ");
        std::io::stderr().flush().ok();

        line.clear();
        if input.read_until(b'\n', &mut line)? == 0 {
            eprintln!();
            return Ok(ExitCode::SUCCESS);
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        if line.last() == Some(&b'\r') {
            line.pop();
        }

        let Some(argv) = split_cmdline(&line) else {
            emit(b"invalid command format '", &line, b"': unclosed quote\n")?;
            continue;
        };
        let prog = &argv[0];

        if prog.is_empty() {
            continue;
        }
        if matches!(prog.as_slice(), b"quit" | b"logout" | b"exit" | b"bye") {
            return Ok(ExitCode::SUCCESS);
        }
        if is_valid_cmd_name(prog) {
            let mut cmd = Command::new(make_cmd(prog));
            for a in &argv[1..] {
                cmd.arg(OsStr::from_bytes(a));
            }
            match cmd.status() {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    emit(b"unrecognized command '", prog, b"'\n")?;
                }
                // Other exec failures are silent (git's RUN_SILENT_EXEC_FAILURE).
                Err(_) => {}
            }
        } else {
            emit(b"invalid command format '", &line, b"'\n")?;
        }
    }
}

/// `shell.c: cd_to_homedir()`. Returns the exit code when it dies.
fn cd_to_homedir() -> Option<ExitCode> {
    let Some(home) = std::env::var_os("HOME") else {
        return Some(die_code(
            "could not determine user's home directory; HOME is unset",
        ));
    };
    if std::env::set_current_dir(&home).is_err() {
        return Some(die_code("could not chdir to user's home directory"));
    }
    None
}

/// `alias.c: split_cmdline()`. `None` is git's `unclosed quote` failure.
///
/// Note that `argv[0]` is seeded before the scan, so the result always holds at
/// least one (possibly empty) element — the behaviour that makes `-c ""` fail
/// as an exec attempt rather than as a parse error.
fn split_cmdline(cmdline: &[u8]) -> Option<Vec<Vec<u8>>> {
    let mut out: Vec<Vec<u8>> = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    let mut quoted = 0u8;
    let mut src = 0usize;

    while src < cmdline.len() {
        let c = cmdline[src];
        if quoted == 0 && is_space(c) {
            out.push(std::mem::take(&mut cur));
            src += 1;
            while src < cmdline.len() && is_space(cmdline[src]) {
                src += 1;
            }
        } else if quoted == 0 && (c == b'\'' || c == b'"') {
            quoted = c;
            src += 1;
        } else if c == quoted {
            quoted = 0;
            src += 1;
        } else {
            // A backslash escapes the next byte unless we are inside '...'.
            if c == b'\\' && quoted != b'\'' {
                src += 1;
                if src >= cmdline.len() {
                    break;
                }
            }
            cur.push(cmdline[src]);
            src += 1;
        }
    }

    if quoted != 0 {
        return None;
    }
    out.push(cur);
    Some(out)
}

/// `quote.c: sq_dequote()` with `next == NULL`: the whole string must be one
/// single-quoted word. `None` is git's `NULL` return.
fn sq_dequote(arg: &[u8]) -> Option<Vec<u8>> {
    if arg.first() != Some(&b'\'') {
        return None;
    }
    let mut dst: Vec<u8> = Vec::new();
    let mut src = 0usize;
    loop {
        src += 1;
        let c = *arg.get(src)?;
        if c != b'\'' {
            dst.push(c);
            continue;
        }
        // We stepped out of the single quotes.
        src += 1;
        match arg.get(src) {
            None => return Some(dst),
            // Backslashed bytes are allowed outside the quotes only when they
            // need escaping and the quoted run resumes immediately after.
            Some(&b'\\') => {
                src += 1;
                let esc = arg.get(src).copied();
                if matches!(esc, Some(b'\'') | Some(b'!')) && arg.get(src + 1) == Some(&b'\'') {
                    dst.push(esc.expect("matched above"));
                    src += 1;
                    continue;
                }
                return None;
            }
            Some(_) => return None,
        }
    }
}

/// `shell.c: is_valid_cmd_name()` — the name must contain no `.` and no `/`.
fn is_valid_cmd_name(cmd: &[u8]) -> bool {
    !cmd.iter().any(|&c| c == b'.' || c == b'/')
}

/// `shell.c: make_cmd()`.
fn make_cmd(prog: &[u8]) -> PathBuf {
    Path::new(COMMAND_DIR).join(OsStr::from_bytes(prog))
}

/// Approximates `access(dir, R_OK | X_OK)`: opening the directory covers read,
/// resolving `dir/.` covers search.
fn readable_and_searchable(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok() && std::fs::metadata(dir.join(".")).is_ok()
}

/// C's `isspace()` in the C locale.
fn is_space(c: u8) -> bool {
    c == b' ' || (0x09..=0x0d).contains(&c)
}

/// Translate a waited-for child's status into our own exit code.
fn exit_code_of(status: std::process::ExitStatus) -> ExitCode {
    match status.code() {
        Some(code) => ExitCode::from(code as u8),
        None => ExitCode::from(128u8.wrapping_add(status.signal().unwrap_or(0) as u8)),
    }
}

/// Write a diagnostic whose middle section is raw, possibly non-UTF-8 bytes.
fn emit(prefix: &[u8], middle: &[u8], suffix: &[u8]) -> Result<()> {
    let mut err = std::io::stderr().lock();
    err.write_all(prefix)?;
    err.write_all(middle)?;
    err.write_all(suffix)?;
    Ok(())
}

/// `usage.c: die()` — `fatal: <msg>` on stderr, exit 128.
fn die_code(msg: &str) -> ExitCode {
    eprintln!("fatal: {msg}");
    ExitCode::from(128)
}

/// [`die_code`] wrapped for the common `return die(...)` form.
fn die(msg: &str) -> Result<ExitCode> {
    Ok(die_code(msg))
}
