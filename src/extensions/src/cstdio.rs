//! C stdio's stdout buffering — the thing that decides what order stock git's
//! stdout and stderr lines come out in when a caller captures both together.
//!
//! git never orders its two streams explicitly. `printf()` writes to the stdio
//! `stdout` FILE and `fprintf(stderr, …)` writes to `stderr`, which C requires
//! to be unbuffered (C99 7.19.3p7: "the standard error stream is not fully
//! buffered"). `stdout` is line buffered *only* "if it can be determined not to
//! refer to an interactive device"; when it is a pipe or a file, stdio picks
//! full buffering and nothing reaches the fd until the buffer fills or `exit()`
//! flushes it. So the same command reorders itself depending on where stdout
//! points, and both orders are "what git does":
//!
//! ```text
//! $ git checkout feature                    # stdout is a tty: line buffered
//! M       README.md                         (stdout, flushed at the newline)
//! Switched to branch 'feature'              (stderr)
//!
//! $ git checkout feature 2>&1 | cat         # stdout is a pipe: fully buffered
//! Switched to branch 'feature'              (stderr, immediate)
//! M       README.md                         (stdout, flushed by exit())
//! ```
//!
//! Rust has no such rule: `std::io::Stdout` is a `LineWriter` whatever fd 1 is,
//! so a port that writes with `println!` always produces the first order. Every
//! captured comparison against stock git then disagrees on any command that
//! writes to both streams — `checkout`/`switch` (`show_local_changes()` on
//! stdout, `Switched to branch …` on stderr) and `merge` (`Updating <a>..<b>` on
//! stdout, a refused checkout's `error: …` on stderr) among them.
//!
//! This module supplies the missing buffer. [`print`] / [`println`] shadow the
//! prelude macros in the modules that `use` them, so a file opts in with one
//! import and keeps its existing call sites:
//!
//! ```ignore
//! use crate::cstdio::{print, println};
//! ```
//!
//! Importing alone changes nothing: writes pass straight through until a command
//! calls [`defer`], which is the port's stand-in for stdio deciding fd 1 is not
//! interactive. A command that wants git's ordering calls it once on entry; the
//! buffer is emptied by [`flush`] after dispatch returns (and by an `atexit`
//! handler, for the paths that leave through `std::process::exit`). Keeping it
//! opt-in is what lets shared helpers — `merge_apply`'s `Auto-merging` lines,
//! `diff_index`'s output — be routed through here without changing the commands
//! that have not been converted: those never arm the buffer, so their writes are
//! unbuffered exactly as before, and no half-converted command can interleave a
//! buffered line ahead of an unbuffered one.
//!
//! Two deliberate simplifications against real stdio:
//!
//! * The buffer is unbounded. stdio flushes when its `st_blksize`-sized buffer
//!   fills, so a git command that writes megabytes to a pipe does interleave with
//!   stderr partway through. Holding everything only ever defers more, and the
//!   commands armed here write a screenful at most.
//! * A write that fails at flush time is dropped rather than reported. That is
//!   what `exit()` does with a stdio buffer it cannot write out, and it is what
//!   git looks like when its stdout is a closed pipe.

use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

/// Bytes written while [`DEFERRED`] is set and no newline has released them.
static BUFFER: Mutex<Vec<u8>> = Mutex::new(Vec::new());

/// Whether some command asked for git's buffering. Set by [`defer`].
static DEFERRED: AtomicBool = AtomicBool::new(false);

/// `_IOLBF` vs `_IOFBF`: stdio line-buffers an interactive stdout and fully
/// buffers everything else. Resolved once — the pager, the only thing that
/// replaces fd 1 mid-process, installs itself before any command runs.
fn interactive() -> bool {
    static TTY: OnceLock<bool> = OnceLock::new();
    *TTY.get_or_init(|| std::io::stdout().is_terminal())
}

/// Arm git's stdout buffering for this process.
///
/// Called on entry to a command whose output order has to match stock git's when
/// both streams are captured. On a terminal this stays line buffered, so an
/// interactive run is unchanged; off a terminal, stdout is held until [`flush`].
pub fn defer() {
    DEFERRED.store(true, Ordering::Relaxed);
    // Every normal exit runs `flush()` from the dispatcher, but `die()`-shaped
    // paths leave through `std::process::exit`, which skips it — and `exit()` is
    // exactly where C would flush the stdio buffer. Registering once, on arming,
    // keeps the handler off processes that never buffer anything.
    static REGISTERED: OnceLock<()> = OnceLock::new();
    REGISTERED.get_or_init(|| {
        // SAFETY: `at_exit` takes no arguments, returns nothing, and only locks a
        // mutex and writes to fd 1 — the same work `flush()` does from the
        // dispatcher.
        unsafe {
            libc::atexit(at_exit);
        }
    });
}

extern "C" fn at_exit() {
    flush();
}

/// Write `bytes` to stdout, through the buffer when it is armed, dropping any
/// write error the way `exit()`'s flush does. What the `print!`/`println!`
/// shims use, since neither has a return value to report through.
pub fn write_bytes(bytes: &[u8]) {
    let _ = write_bytes_io(bytes);
}

/// [`write_bytes`] for a caller that still reports a failed write — the
/// unbuffered path keeps returning `EPIPE` so `git <cmd> | head` exits through
/// [`crate::sigpipe`] as before. A buffered write cannot fail here; its error, if
/// any, belongs to [`flush`], which discards it.
pub fn write_bytes_io(bytes: &[u8]) -> std::io::Result<()> {
    if !DEFERRED.load(Ordering::Relaxed) {
        let mut out = std::io::stdout().lock();
        out.write_all(bytes)?;
        return out.flush();
    }
    let mut buf = BUFFER.lock().unwrap_or_else(|e| e.into_inner());
    buf.extend_from_slice(bytes);
    // `_IOLBF`: everything up to and including the last newline goes out now, a
    // trailing partial line waits — which is what makes an interactive `git
    // checkout` print its `show_local_changes()` block before `Switched to …`.
    if interactive() {
        if let Some(nl) = buf.iter().rposition(|&b| b == b'\n') {
            let tail = buf.split_off(nl + 1);
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(&buf);
            let _ = out.flush();
            *buf = tail;
        }
    }
    Ok(())
}

/// The `exit()`-time flush of the stdio buffer. Idempotent, so the dispatcher and
/// the `atexit` handler can both call it.
pub fn flush() {
    let mut buf = BUFFER.lock().unwrap_or_else(|e| e.into_inner());
    if buf.is_empty() {
        return;
    }
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(&buf);
    let _ = out.flush();
    buf.clear();
}

/// `start_command()`'s `fflush(NULL)` (run-command.c:743), which git runs
/// immediately before every `fork()`.
///
/// Without it a child's writes — which go straight at the inherited fd — would
/// overtake everything the parent has buffered so far, so `git merge -s resolve`
/// would print the `Auto-merging <path>` line its spawned `merge-one-file`
/// produces *before* the `Trying simple merge.` line the parent produced first.
/// Call this at any site that spawns a child while the buffer may be armed.
pub fn before_spawn() {
    flush();
}

/// Formatting entry point for the [`print`] / [`println`] macros.
pub fn write_args(args: std::fmt::Arguments<'_>) {
    match args.as_str() {
        Some(literal) => write_bytes(literal.as_bytes()),
        None => write_bytes(std::fmt::format(args).as_bytes()),
    }
}

/// `print!` routed through git's stdout buffer.
macro_rules! cstdio_print {
    ($($arg:tt)*) => { $crate::cstdio::write_args(format_args!($($arg)*)) };
}

/// `println!` routed through git's stdout buffer.
macro_rules! cstdio_println {
    () => { $crate::cstdio::write_bytes(b"\n") };
    ($($arg:tt)*) => {
        $crate::cstdio::write_args(format_args!("{}\n", format_args!($($arg)*)))
    };
}

pub(crate) use cstdio_print as print;
pub(crate) use cstdio_println as println;
