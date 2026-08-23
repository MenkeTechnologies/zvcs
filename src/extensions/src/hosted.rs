//! Running zvcs inside somebody else's process.
//!
//! The `git` binary owns its process: it can `exit()` from anywhere, leave the
//! working directory wherever `-C` put it, and let the kernel reclaim
//! everything. A host that dispatches `git` as one command among many — the
//! `git` shell builtin in zshrs-native, where there is no fork and no exec —
//! can afford none of that. This module is the difference between the two.
//!
//! # Leaving early
//!
//! git exits from deep inside rendering loops. `sigpipe::exit_broken_pipe` is
//! reached from seven places in `log`, `show` and `format_patch`; `advice`
//! dies from a config read; five porcelain verbs exit 128 on an internal
//! failure. Every one of them is `-> !` and none has a caller prepared for a
//! return value, so threading `Result` up from them would mean rewriting git's
//! whole log pipeline.
//!
//! Instead [`exit`] keeps `process::exit` when zvcs owns the process, and
//! unwinds when it does not. [`run`] catches the unwind and turns it back into
//! the status the exit asked for. This is what unwinding is for; it costs
//! nothing on the path that does not take it, and destructors still run, so an
//! index lock held by a `RepoLock` is released on the way out.
//!
//! # Process state
//!
//! `git -C <dir>` chdirs, and several globals reach a subprocess through the
//! environment. In our own process that is the point. In a host it is a side
//! effect nobody asked for — a shell whose working directory silently moved
//! because a command it ran used `-C` is broken. [`run`] restores the working
//! directory on the way out, whichever way it leaves.

use std::cell::Cell;
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::Once;

thread_local! {
    /// True while this thread is running a hosted invocation.
    ///
    /// Thread-local rather than global: a host may run zvcs on a worker while
    /// other threads do unrelated work, and only the invocation's own thread
    /// may unwind instead of exiting.
    static HOSTED: Cell<bool> = const { Cell::new(false) };
}

/// The payload [`exit`] unwinds with, carrying the status the caller wanted.
struct HostedExit(i32);

/// Whether this thread is inside [`run`].
pub fn is_hosted() -> bool {
    HOSTED.with(Cell::get)
}

/// git's `exit(code)`, in a form a host process survives.
///
/// Outside a host this is `std::process::exit` exactly. Inside one it unwinds
/// to the [`run`] that started the invocation, which returns `code`.
pub fn exit(code: i32) -> ! {
    if is_hosted() {
        panic::panic_any(HostedExit(code));
    }
    std::process::exit(code)
}

/// Teach the panic hook to stay quiet about [`HostedExit`].
///
/// An ordinary `panic_any` prints "thread panicked at …" before unwinding,
/// which would put noise on the host's stderr every time `git log | head`
/// closes its pipe. The hook delegates to whatever was installed before it —
/// the host's own hook included — for every other payload, so a real panic
/// still reports exactly as it would have.
fn install_quiet_hook() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            if info.payload().is::<HostedExit>() {
                return;
            }
            previous(info);
        }));
    });
}

/// The binary to spawn when zvcs re-runs itself as `git`.
///
/// git forks a `git` child for a good number of jobs — `status` asks a child
/// for the submodule summary, `submodule update` fetches through one, `rebase`
/// drives `am` and `commit` through children, `credential-cache` daemonises
/// into one — and this port keeps that shape, spawning itself. Outside a host
/// "itself" is [`std::env::current_exe`] and there is nothing to think about.
///
/// Inside a host there is: `current_exe()` is the HOST. In zshrs-native it is
/// the shell, so `Command::new(current_exe()).args(["submodule", "summary"])`
/// runs *the shell* with those words and it answers
/// `zshrs: can't open input file: submodule`. Every self-exec site has to ask
/// for the git binary by name instead, which is what this returns:
///
///  1. `$ZVCS_GIT_EXE`, for a host that knows exactly which binary it wants;
///  2. the first executable `git` on `PATH` that is not the host binary
///     itself — the guard matters because the host may well BE on `PATH` under
///     a name that resolves back here, and spawning it would loop;
///  3. failing both, `NotFound`, which every caller already handles by
///     degrading (no submodule summary, no daemonised credential cache)
///     rather than by pretending the child ran.
///
/// Not cached: `PATH` is the host's live environment and a shell changes it.
pub fn git_exe() -> std::io::Result<PathBuf> {
    if !is_hosted() {
        return std::env::current_exe();
    }
    if let Some(explicit) = std::env::var_os("ZVCS_GIT_EXE") {
        let path = PathBuf::from(explicit);
        if is_executable(&path) {
            return Ok(path);
        }
    }
    let own = std::env::current_exe().ok().and_then(|p| p.canonicalize().ok());
    let path = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join("git");
        if !is_executable(&candidate) {
            continue;
        }
        // The host under another name is not a git to spawn; skipping it is
        // what keeps a `git` symlink that points back at the host from
        // re-entering this process's own command line forever.
        if candidate.canonicalize().ok() == own {
            continue;
        }
        return Ok(candidate);
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "no `git` on PATH to run as a child (set ZVCS_GIT_EXE)",
    ))
}

/// A path that exists, is a file, and carries an execute bit.
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Run one hosted zvcs invocation and return the status it left with.
///
/// Covers the three things the process boundary used to do for free: an
/// [`exit`] from anywhere becomes a return value, a panic anywhere becomes a
/// status instead of taking the host down with it, and the working directory
/// is put back where the host had it.
///
/// A panic that is not a [`HostedExit`] still prints through the host's hook
/// and yields 128 — git's own status for an internal failure.
pub fn run<F>(f: F) -> i32
where
    F: FnOnce() -> i32,
{
    install_quiet_hook();

    let cwd: Option<PathBuf> = std::env::current_dir().ok();
    let previously = HOSTED.replace(true);

    let outcome = panic::catch_unwind(AssertUnwindSafe(f));

    // The pager owns fd 1 (and sometimes fd 2) between `maybe_setup` and
    // `finish`, and the ordinary path through `run_command` closes that window
    // itself. An `exit` from inside a paged verb does not: it unwinds straight
    // past the teardown, and in a host that would leave the shell writing into
    // a pipe whose reader has gone — `git log`, quit `less` with `q`, and the
    // terminal is silent from then on. `finish` is a no-op when no pager is
    // installed, so this is the same guarantee as the working-directory
    // restore below: whichever way the invocation left, the host gets its
    // descriptors back.
    crate::pager::finish();

    HOSTED.set(previously);
    if let Some(dir) = cwd {
        let _ = std::env::set_current_dir(dir);
    }

    match outcome {
        Ok(code) => code,
        Err(payload) => match payload.downcast::<HostedExit>() {
            Ok(exit) => exit.0,
            // A real panic. The hook has already reported it; 128 is what git
            // uses for "something went wrong inside git", which is precisely
            // what happened.
            Err(_) => 128,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_from_inside_becomes_a_return_value() {
        assert_eq!(run(|| exit(3)), 3);
    }

    #[test]
    fn a_normal_return_is_untouched() {
        assert_eq!(run(|| 0), 0);
    }

    #[test]
    fn a_panic_becomes_git_s_internal_failure_status() {
        assert_eq!(run(|| panic!("boom")), 128);
    }

    #[test]
    fn the_flag_is_off_again_afterwards() {
        assert!(!is_hosted());
        assert_eq!(run(|| if is_hosted() { 1 } else { 0 }), 1);
        assert!(!is_hosted());
    }

    /// Outside a host, the spawn target is this process — the identity git
    /// itself relies on, and the property that keeps the standalone `git`
    /// binary's behaviour byte-identical after the hosted path was added.
    #[test]
    fn the_spawn_target_outside_a_host_is_this_process() {
        assert!(!is_hosted());
        assert_eq!(git_exe().unwrap(), std::env::current_exe().unwrap());
    }

    /// The executable probe is what stops a directory named `git` on `PATH`,
    /// or a non-executable file, from being handed to `Command::new` — which
    /// would fail at spawn time with a far less obvious message.
    #[test]
    fn only_an_executable_file_is_a_spawn_candidate() {
        let dir = std::env::temp_dir().join("zvcs_hosted_exe_probe");
        let _ = std::fs::create_dir_all(&dir);
        let plain = dir.join("not-executable");
        std::fs::write(&plain, b"#!/bin/sh\n").unwrap();

        assert!(!is_executable(&dir), "a directory is not a spawn target");
        assert!(!is_executable(&plain));
        assert!(!is_executable(&dir.join("absent")));

        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&plain).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&plain, perms).unwrap();
        assert!(is_executable(&plain));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_working_directory_is_restored() {
        let before = std::env::current_dir().unwrap();
        let code = run(|| {
            std::env::set_current_dir(std::env::temp_dir()).unwrap();
            0
        });
        assert_eq!(code, 0);
        assert_eq!(std::env::current_dir().unwrap(), before);
    }
}
