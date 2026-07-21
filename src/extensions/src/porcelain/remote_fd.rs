//! `git remote-fd` — reflect a smart-transport stream back to the caller.
//!
//! A faithful port of `builtin/remote-fd.c` together with the one function it
//! leans on, `transport-helper.c::bidirectional_transfer_loop` (and its
//! `unidirectional_transfer` machinery: `udt_do_read`, `udt_do_write`,
//! `udt_close_if_finished`, `udt_copy_task_routine`, `tloop_join`,
//! `tloop_spawnwait_tasks` in its `NO_PTHREADS`-off, i.e. threaded, form).
//!
//! This command deliberately touches **no** repository: it is a remote helper
//! that shovels bytes between the caller's stdin/stdout and a pair of inherited
//! file descriptors. There is therefore no gitoxide involvement — the C original
//! has none either, and adding any would be a divergence.
//!
//! ### The URL
//!
//! git invokes a `<transport>::<address>` helper as `git remote-<transport>
//! <remote> <address>`, so for the URL `fd::7,8/bar` this command receives
//! `<remote>` and `7,8/bar`. The address is parsed exactly as the C does, with
//! `strtoul(…, 10)` semantics reproduced in [`strtoul`]:
//!
//!   * `<infd>` — one bidirectional socket; the output descriptor is the same fd
//!   * `<infd>,<outfd>` — inbound pipe and outbound pipe
//!   * an optional `/<anything>` tail, ignored (it exists only so the URL reads
//!     well when displayed)
//!
//! ### Covered (byte-identical stdout/stderr and exit code against stock git)
//!
//! * `git remote-fd <remote> <url>` — the remote-helper command loop on stdin:
//!   `capabilities` answers `*connect\n\n`, `connect <service>` answers `\n` and
//!   then enters the transfer loop, EOF returns quietly with exit 0, and
//!   anything else is `fatal: Bad command: <line>` with exit 128. Trailing
//!   whitespace is stripped from each line first, so a bare `connect` (no
//!   service argument) is a bad command, as it is for git.
//! * the bidirectional transfer loop itself: two threads, a 65536-byte buffer
//!   each, stdin → `<outfd>` and `<infd>` → stdout, with EOF on a source
//!   flushing the buffer and then closing the destination — or, when the two
//!   descriptors are the same fd, shutting down only its write half, so the peer
//!   sees EOF while the read half stays usable.
//! * `Bad URL syntax` on stderr with exit 128 for an address that is not a
//!   number, has a trailing `,` with no second number, or carries any tail that
//!   does not begin with `/`.
//! * `-h` as the only argument — usage on stdout, exit 129; any other argument
//!   count than exactly `<remote> <url>` — the same usage on stderr, exit 129.
//! * a failed copy — exit 128 with
//!   `fatal: Copying data between file descriptors failed`, preceded by git's
//!   `error: … thread failed` line.
//!
//! ### Honest limitations
//!
//! * On a read/write failure git prints `error: read(remote input) failed: <s>`
//!   where `<s>` is `strerror(errno)`; this prints Rust's `io::Error` rendering,
//!   which appends ` (os error N)` to the same text. Only that stderr diagnostic
//!   differs — the exit code, the subsequent `error: … thread failed` line, and
//!   the final `fatal:` line all match.
//! * The command line is read one byte at a time straight from descriptor 0
//!   rather than through a buffered reader. git uses `fgets`, whose stdio buffer
//!   could in principle swallow payload bytes that the transfer loop then never
//!   sees; reading unbuffered cannot lose data. The two agree for every real
//!   caller, which waits for the `\n` acknowledgement before sending payload.

use anyhow::Result;
use std::fs::File;
use std::io::{Read, Write};
use std::mem::ManuallyDrop;
use std::net::Shutdown;
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::process::ExitCode;

/// Stock git's usage line for this command, byte-for-byte. Stdout on a bare
/// `-h`, stderr on any argument-count error; both exit 129.
const USAGE: &str = "usage: git remote-fd <remote> <url>\n";

/// `remote-fd.c`'s `MAXCOMMAND`. `fgets` is handed `MAXCOMMAND - 1` as its size
/// and so stores at most `MAXCOMMAND - 2` bytes plus the terminating NUL.
const MAXCOMMAND: usize = 4096;

/// `transport-helper.c`'s `BUFFERSIZE` — the per-direction copy buffer.
const BUFFERSIZE: usize = 65536;

/// `git remote-fd` — see the module docs for the covered behaviour.
pub fn remote_fd(args: &[String]) -> Result<ExitCode> {
    // `args` mirrors C's `argv`: argv[0] is the command, argv[1] the remote name
    // (unused — the fd pair carries everything), argv[2] the address.

    // `show_usage_if_asked()`: a lone `-h` prints the usage on stdout, exit 129.
    if args.len() == 2 && args[1] == "-h" {
        print!("{USAGE}");
        std::io::stdout().flush()?;
        return Ok(ExitCode::from(129));
    }
    if args.len() != 3 {
        eprint!("{USAGE}");
        return Ok(ExitCode::from(129));
    }

    let url = args[2].as_bytes();

    // input_fd = (int)strtoul(argv[2], &end, 10);
    let (value, end) = strtoul(url);
    let input_fd = value as u32 as RawFd;
    // if ((end == argv[2]) || (*end != ',' && *end != '/' && *end)) die(…);
    // `end == argv[2]` means no digits were converted; a NUL byte (end of
    // string) is the one remaining accepted terminator.
    let sep = url.get(end).copied();
    if end == 0 || !matches!(sep, None | Some(b',') | Some(b'/')) {
        return Ok(die("Bad URL syntax"));
    }

    let output_fd = match sep {
        // '/<anything>' or end of string: one bidirectional descriptor.
        None | Some(b'/') => input_fd,
        // ',<outfd>[/<anything>]': separate inbound and outbound pipes.
        _ => {
            let rest = &url[end + 1..];
            let (value2, end2) = strtoul(rest);
            if end2 == 0 || !matches!(rest.get(end2).copied(), None | Some(b'/')) {
                return Ok(die("Bad URL syntax"));
            }
            value2 as u32 as RawFd
        }
    };

    command_loop(input_fd, output_fd)
}

/// `remote-fd.c::command_loop` — the remote-helper protocol on stdin.
///
/// Returns as soon as stdin reaches EOF, or after one `connect` has run the
/// transfer loop to completion; every other line is fatal.
fn command_loop(input_fd: RawFd, output_fd: RawFd) -> Result<ExitCode> {
    loop {
        let mut line = match read_command() {
            Ok(Some(line)) => line,
            // fgets returned NULL without an error: clean EOF, exit 0.
            Ok(None) => return Ok(ExitCode::SUCCESS),
            // ferror(stdin) — git's die("Input error").
            Err(_) => return Ok(die("Input error")),
        };

        // "Strip end of line characters." — C's isspace(), from the tail.
        while line.last().is_some_and(|&b| is_c_space(b)) {
            line.pop();
        }

        if line == b"capabilities" {
            let mut out = std::io::stdout();
            out.write_all(b"*connect\n\n")?;
            out.flush()?;
        } else if line.starts_with(b"connect ") {
            let mut out = std::io::stdout();
            out.write_all(b"\n")?;
            out.flush()?;
            if bidirectional_transfer_loop(input_fd, output_fd) {
                return Ok(die("Copying data between file descriptors failed"));
            }
            return Ok(ExitCode::SUCCESS);
        } else {
            let mut err = std::io::stderr();
            err.write_all(b"fatal: Bad command: ")?;
            err.write_all(&line)?;
            err.write_all(b"\n")?;
            return Ok(ExitCode::from(128));
        }
    }
}

/// One `fgets(buffer, MAXCOMMAND - 1, stdin)` worth of input, as a byte string.
///
/// `Ok(None)` is `fgets` returning NULL on a clean EOF; `Err` is `ferror(stdin)`,
/// which the caller turns into git's `die("Input error")`.
///
/// Descriptor 0 is read directly and unbuffered: whatever this function does not
/// consume must still be visible to the transfer loop, which reads the same
/// descriptor. Only the NUL-truncation of C strings is reproduced, since the
/// line is subsequently compared with `strcmp`/`starts_with`.
fn read_command() -> std::io::Result<Option<Vec<u8>>> {
    let mut stdin = ManuallyDrop::new(unsafe { File::from_raw_fd(0) });
    let mut line = Vec::with_capacity(128);
    let mut byte = [0u8; 1];

    while line.len() < MAXCOMMAND - 2 {
        match stdin.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => {
                line.push(byte[0]);
                if byte[0] == b'\n' {
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }

    if line.is_empty() {
        return Ok(None);
    }
    // The C buffer is a NUL-terminated string, so a stray NUL ends the command.
    if let Some(nul) = line.iter().position(|&b| b == 0) {
        line.truncate(nul);
    }
    Ok(Some(line))
}

/// `transport-helper.c::bidirectional_transfer_loop` — returns true on failure.
///
/// Two threads run concurrently, matching the pthreads build of the original:
///
///   * *git to program*: stdin → `output`. Its destination is a socket exactly
///     when the caller gave a single descriptor, in which case the write half is
///     shut down rather than closed at EOF.
///   * *program to git*: `input` → stdout. Its destination, descriptor 1, is
///     never a socket and so is closed outright at EOF, which is what signals
///     end-of-stream to the process that spawned this helper.
///
/// Neither task closes its source, and the threads are joined in creation order,
/// both as in the original.
fn bidirectional_transfer_loop(input: RawFd, output: RawFd) -> bool {
    let same = input == output;

    let gtp = std::thread::spawn(move || {
        copy_task(Transfer {
            src: 0,
            dest: output,
            dest_is_sock: same,
            src_name: "stdin",
            dest_name: "remote output",
        })
    });
    let ptg = std::thread::spawn(move || {
        copy_task(Transfer {
            src: input,
            dest: 1,
            dest_is_sock: false,
            src_name: "remote input",
            dest_name: "stdout",
        })
    });

    let mut failed = tloop_join(gtp, "Git to program copy");
    failed |= tloop_join(ptg, "Program to git copy");
    failed
}

/// `tloop_join` — true when the task failed (or its thread died outright, which
/// `pthread_join` would report the same way).
fn tloop_join(handle: std::thread::JoinHandle<bool>, name: &str) -> bool {
    let ok = handle.join().unwrap_or(false);
    if !ok {
        eprintln!("error: {name} thread failed");
    }
    !ok
}

/// One direction of the copy, mirroring `struct unidirectional_transfer`.
///
/// `src_is_sock` is omitted: the threaded `udt_copy_task_routine` never touches
/// the source descriptor, so the field is dead in this build.
struct Transfer {
    src: RawFd,
    dest: RawFd,
    dest_is_sock: bool,
    src_name: &'static str,
    dest_name: &'static str,
}

/// `udt_copy_task_routine` — true on success, false once a read or write failed.
///
/// The three states of the original are folded into `flushing` (the source hit
/// EOF, drain what is buffered) and the loop's own exit (`SSTATE_FINISHED`).
fn copy_task(t: Transfer) -> bool {
    let mut src = ManuallyDrop::new(unsafe { File::from_raw_fd(t.src) });
    let mut dest = ManuallyDrop::new(unsafe { File::from_raw_fd(t.dest) });
    let mut buf = vec![0u8; BUFFERSIZE];
    let mut bufuse = 0usize;
    let mut flushing = false;

    loop {
        // udt_do_read: never overfills, EOF switches to flushing.
        if !flushing && bufuse < BUFFERSIZE {
            match src.read(&mut buf[bufuse..]) {
                Ok(0) => flushing = true,
                Ok(n) => bufuse += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => {
                    eprintln!("error: read({}) failed: {e}", t.src_name);
                    return false;
                }
            }
        }

        // udt_do_write: a short write leaves the remainder for the next round.
        if bufuse > 0 {
            match dest.write(&buf[..bufuse]) {
                Ok(n) => {
                    buf.copy_within(n..bufuse, 0);
                    bufuse -= n;
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => {
                    eprintln!("error: write({}) failed: {e}", t.dest_name);
                    return false;
                }
            }
        }

        // udt_close_if_finished.
        if flushing && bufuse == 0 {
            if t.dest_is_sock {
                // shutdown(dest, SHUT_WR): the peer sees EOF while the read half
                // of the shared descriptor stays open for the other direction.
                // The address family is irrelevant to the syscall, so wrapping
                // the descriptor as a `UnixStream` is only a way to reach it.
                let sock = ManuallyDrop::new(unsafe { UnixStream::from_raw_fd(t.dest) });
                let _ = sock.shutdown(Shutdown::Write);
            } else {
                // close(dest) — take ownership back so the drop closes it.
                drop(unsafe { File::from_raw_fd(t.dest) });
            }
            return true;
        }
    }
}

/// `die(msg)` — `fatal: <msg>` on stderr, exit 128.
fn die(msg: &str) -> ExitCode {
    eprintln!("fatal: {msg}");
    ExitCode::from(128)
}

/// C's `isspace()` in the default locale.
fn is_c_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// `strtoul(s, &end, 10)`, returning the value and the offset `end` sits at.
///
/// Reproduced rather than delegated to Rust's integer parser because the exact
/// stopping point drives the URL syntax check, and because the accepted forms
/// differ: leading whitespace is skipped, a sign is honoured (`-1` parses to
/// `ULONG_MAX`, which the caller truncates to the descriptor `-1`), and overflow
/// saturates at `ULONG_MAX` instead of failing.
///
/// An offset of 0 is C's "no conversion performed", where `end` is left equal to
/// the start of the string — the condition `remote-fd.c` tests for.
fn strtoul(s: &[u8]) -> (u64, usize) {
    let mut i = 0;
    while i < s.len() && is_c_space(s[i]) {
        i += 1;
    }
    let mut negative = false;
    if matches!(s.get(i), Some(b'+') | Some(b'-')) {
        negative = s[i] == b'-';
        i += 1;
    }

    let first_digit = i;
    let mut value: u64 = 0;
    let mut overflowed = false;
    while i < s.len() && s[i].is_ascii_digit() {
        let digit = u64::from(s[i] - b'0');
        match value.checked_mul(10).and_then(|v| v.checked_add(digit)) {
            Some(v) => value = v,
            None => overflowed = true,
        }
        i += 1;
    }
    if i == first_digit {
        return (0, 0);
    }

    if overflowed {
        // ERANGE: strtoul yields ULONG_MAX whatever the sign was.
        value = u64::MAX;
    } else if negative {
        value = value.wrapping_neg();
    }
    (value, i)
}

#[cfg(test)]
mod tests {
    use super::strtoul;

    #[test]
    fn strtoul_matches_c_stopping_points() {
        // Plain number consumes everything.
        assert_eq!(strtoul(b"17"), (17, 2));
        // The separator is left for the caller to inspect.
        assert_eq!(strtoul(b"7,8"), (7, 1));
        assert_eq!(strtoul(b"17/foo"), (17, 2));
        // No digits at all: "no conversion performed", end == start.
        assert_eq!(strtoul(b"abc"), (0, 0));
        assert_eq!(strtoul(b""), (0, 0));
        assert_eq!(strtoul(b","), (0, 0));
        // Leading whitespace and a sign are both accepted by strtoul.
        assert_eq!(strtoul(b"  8"), (8, 3));
        assert_eq!(strtoul(b"-1"), (u64::MAX, 2));
        // Overflow saturates at ULONG_MAX rather than failing, which is why
        // `git remote-fd origin 99999999999999999999` is accepted (as fd -1).
        assert_eq!(strtoul(b"99999999999999999999"), (u64::MAX, 20));
        assert_eq!(strtoul(b"-1").0 as u32 as i32, -1);
    }
}
