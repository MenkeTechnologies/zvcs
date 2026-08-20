//! `SIGPIPE` disposition on the output path, matching git's observable behavior.
//!
//! Rust's runtime sets `SIGPIPE` to `SIG_IGN` before `main`, so a write to a
//! closed pipe returns `EPIPE` instead of killing the process. That default is
//! what the child-stdin writers in this crate rely on — `gitsig` writing a
//! payload to an `ssh-keygen` that already exited, `hooks` feeding a hook that
//! closed early, `receive_pack`, `am`, `imap_send` — so it is left in place.
//!
//! It is wrong for stdout. Stock git leaves `SIGPIPE` at its default and dies
//! from the signal when the reader of its stdout goes away, which the shell
//! reports as 128+13:
//!
//! ```text
//! git ls-files | head -1   →  exit 141, nothing on stderr
//! ```
//!
//! Under `SIG_IGN` the same pipeline instead surfaced an `EPIPE` up the error
//! path and printed `zvcs: ls-files: Broken pipe (os error 32)` with exit 1 —
//! spurious diagnostics plus a failing status for what git calls a normal stop.
//! Commands that caught it themselves had the mirror-image problem, returning 0
//! where git returns 141.
//!
//! A pager is the one case where git does not die. When git owns the pager child
//! and the user quits it early, git exits 0 and says nothing:
//!
//! ```text
//! git -c core.pager='head -1' ls-files  →  exit 0, nothing on stderr
//! ```
//!
//! So the exit taken here depends on who closed the pipe: our own pager (a
//! normal end to the session, 0) or an unrelated downstream reader (git's 141).

use std::error::Error;

/// Does `err`, or anything it wraps, report a closed pipe?
///
/// The error arriving at the top-level handler has usually been boxed and
/// annotated on its way up, so the `io::Error` is rarely the outermost layer;
/// the whole `source()` chain is walked to find it.
pub fn is_broken_pipe(err: &(dyn Error + 'static)) -> bool {
    let mut cur = Some(err);
    while let Some(e) = cur {
        if let Some(io) = e.downcast_ref::<std::io::Error>() {
            if io.kind() == std::io::ErrorKind::BrokenPipe {
                return true;
            }
        }
        cur = e.source();
    }
    false
}

/// Leave the process the way git leaves it when its output pipe closes.
///
/// With our own pager installed this is the user quitting the pager: exit 0,
/// silently. Otherwise the reader on the far side of stdout is gone and git
/// would be dead from `SIGPIPE` already — so the signal is restored to its
/// default and re-raised against ourselves, making the wait status a real
/// signal death rather than an ordinary exit. Anything watching the process
/// (a shell's `$?`, `WIFSIGNALED`) then sees exactly what it sees for git.
pub fn exit_broken_pipe() -> ! {
    if crate::pager::is_active() {
        crate::pager::finish();
        crate::hosted::exit(0);
    }
    // Hosted, the signal death is not ours to stage: restoring SIG_DFL and
    // raising SIGPIPE would kill the host — an interactive shell, for
    // `git log | head`. Report 141 instead, the status a shell records for a
    // child that died on SIGPIPE, which is the number the staged death exists
    // to produce in the first place.
    if crate::hosted::is_hosted() {
        crate::hosted::exit(141);
    }
    // SAFETY: both calls are async-signal-safe and this thread is on its way
    // out. `raise` does not return once the default disposition is restored.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
        libc::raise(libc::SIGPIPE);
    }
    // Not reached: `raise` above terminates the process. Kept so the function
    // is `!` without an `unreachable!` panic, and so a platform that somehow
    // blocks the signal still reports git's status instead of falling through.
    crate::hosted::exit(141);
}
