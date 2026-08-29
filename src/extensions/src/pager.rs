//! Output paging, a faithful port of git's `pager.c` / `setup_pager()`.
//!
//! When stdout is a terminal and the subcommand is one git would page (or `-p`
//! forces it), we spawn `$GIT_PAGER` / `core.pager` / `$PAGER` / `less` and
//! `dup2` its stdin over fd 1 (and fd 2 when stderr is a tty), so every write a
//! command makes to stdout flows through the pager. Git's `LESS=FRX` default
//! makes short output pass straight through — the pager only takes over when the
//! content exceeds one screen, which is exactly the "small screen" case where
//! stock git pages and zvcs previously did not.
//!
//! The choice is made once per process in [`maybe_setup`] before dispatch, and
//! torn down in [`finish`] after: flush, close the fds so the pager reads EOF,
//! then wait for it so control returns to the shell only after the user quits.

use std::io::{IsTerminal, Write};
use std::os::unix::io::{AsRawFd, RawFd};
use std::process::{Child, Stdio};
use std::sync::Mutex;

/// The subcommands git pages by default when stdout is a terminal — the read /
/// list verbs whose builtins call `setup_pager()` (or carry `USE_PAGER`), plus
/// the zvcs superset log viewer. A per-command `pager.<cmd>` config value or a
/// command-line `-p` / `-P` overrides membership here.
const DEFAULT_PAGER_CMDS: &[&str] = &[
    // git porcelain that pages when stdout is a tty
    "log",
    "show",
    "diff",
    "whatchanged",
    "reflog",
    "shortlog",
    "range-diff",
    "grep",
    "blame",
    "annotate",
    "branch",
    "tag",
    "config",
    "help",
    // zvcs superset viewers
    "zlog",
];

/// The live pager child plus whether we also redirected stderr onto it, so
/// [`finish`] closes exactly the fds it swapped.
struct Pager {
    child: Child,
    stderr_redirected: bool,
    /// The host's own fd 1 and fd 2, parked out of the way while the pager owns
    /// the low numbers. `None` when zvcs owns the process: git closes fd 1 to
    /// end the pager session and then exits, so there is nothing to give back.
    ///
    /// Hosted there is. `finish` used to close fd 1 outright, which inside
    /// zshrs-native closed *the shell's* stdout — every command after a paged
    /// `git status` wrote to a descriptor that was gone, and the terminal went
    /// silent for the rest of the session.
    saved_stdout: Option<RawFd>,
    /// The same for fd 2, parked only when the pager took stderr as well.
    saved_stderr: Option<RawFd>,
    /// `$GIT_PAGER_IN_USE` as the host had it, restored with the descriptors —
    /// it is read by color and column-width decisions, so leaving it set makes
    /// every later command in that shell believe it is writing into a pager.
    prev_pager_in_use: Option<Option<std::ffi::OsString>>,
}

static PAGER: Mutex<Option<Pager>> = Mutex::new(None);

/// `term_columns()` (`pager.c:203`): the cached `$COLUMNS` when it `atoi`s to a
/// positive number, else the `TIOCGWINSZ` probe on fd 1, else 80.
///
/// The probe is deliberately absent — the vendored crates expose no `ioctl`, and
/// every consumer here (the `--stat` geometry, `column`'s layout,
/// `%<|(-<N>)` padding) is compared against a captured pipe, where the C's
/// `ioctl(1, …)` fails and it falls through to the same 80. A tty-attached run
/// with `COLUMNS` unset therefore uses 80 where git would use the window width.
///
/// The parse is `atoi()`, not `strtol` with error checking and not "the leading
/// digit run": optional whitespace, an optional sign, then digits, with whatever
/// follows ignored. `COLUMNS=+100` is 100 — a digits-only reader answers 80 —
/// `COLUMNS=1-2` is 1, and `COLUMNS=-5` is -5, which is not positive and so
/// falls through to 80. Three copies of this had disagreed on exactly those
/// cases.
pub(crate) fn term_columns() -> i64 {
    if let Ok(value) = std::env::var("COLUMNS") {
        if let Some(n) = atoi(value.as_bytes()) {
            if n > 0 {
                return n;
            }
        }
    }
    80
}

/// C's `atoi()` over bytes: `isspace`* `[+-]?` `[0-9]*`, trailing junk ignored.
/// `None` is C's "no conversion performed", which it reports as 0 and every
/// caller here treats the same way.
fn atoi(s: &[u8]) -> Option<i64> {
    let mut i = 0;
    while matches!(s.get(i), Some(b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')) {
        i += 1;
    }
    let negative = match s.get(i) {
        Some(b'-') => {
            i += 1;
            true
        }
        Some(b'+') => {
            i += 1;
            false
        }
        _ => false,
    };
    let start = i;
    let mut n: i64 = 0;
    while let Some(d) = s.get(i).filter(|c| c.is_ascii_digit()) {
        n = n.saturating_mul(10).saturating_add(i64::from(d - b'0'));
        i += 1;
    }
    if i == start {
        return None;
    }
    Some(if negative { -n } else { n })
}

/// Decide whether to page `cmd` and, if so, install the pager over stdout.
///
/// `forced` carries the command-line choice: `Some(true)` for `-p`/`--paginate`,
/// `Some(false)` for `-P`/`--no-pager`, `None` when neither was given. The
/// command line wins over config, which wins over the default set — matching
/// git's precedence, where the config check only runs while `use_pager == -1`.
pub fn maybe_setup(cmd: &str, forced: Option<bool>) {
    // `-P`/`--no-pager`, or output is not a terminal: never page.
    if forced == Some(false) || !std::io::stdout().is_terminal() {
        return;
    }
    // An ancestor already set up a pager we are writing into.
    if env_flag("GIT_PAGER_IN_USE") {
        return;
    }

    // Resolve config from the repo when we are in one (honors repo-scoped
    // `core.pager` / `pager.<cmd>`); fall back to global+env otherwise.
    let repo = crate::setup::discover().ok();
    let cfg = repo.as_ref().map(|r| r.config_snapshot());

    let want = match forced {
        Some(true) => true,
        _ => match cfg.as_ref().and_then(|c| c.boolean(&format!("pager.{cmd}"))) {
            Some(explicit) => explicit,
            None => DEFAULT_PAGER_CMDS.contains(&cmd),
        },
    };
    if !want {
        return;
    }

    let program = resolve_pager(cfg.as_ref());
    // Empty or `cat` means "no pager", exactly as git's `git_pager()` returns.
    if program.is_empty() || program == "cat" {
        return;
    }
    spawn(&program);
}

/// git's `git_pager()` program chain: `$GIT_PAGER`, then `core.pager`, then
/// `$PAGER`, then the compiled-in `less`.
pub(crate) fn resolve_pager(cfg: Option<&gix::config::Snapshot<'_>>) -> String {
    if let Some(p) = env_nonempty("GIT_PAGER") {
        return p;
    }
    if let Some(p) = cfg.and_then(|c| c.string("core.pager")) {
        return p.to_string();
    }
    if let Some(p) = env_nonempty("PAGER") {
        return p;
    }
    "less".into()
}

/// Spawn the pager and redirect our stdout — and stderr when it is a tty — onto
/// it. `prepare_pager_args()` pushes the pager string as the whole argv of a
/// `use_shell` child, so `core.pager = "less -S"` and pipelines go through the
/// shell while a bare `less` is exec'd directly.
fn spawn(program: &str) {
    let mut cmd = crate::external::prepare_shell_cmd_str(program, crate::external::NO_ARGS);
    cmd.stdin(Stdio::piped());
    // git's build-time PAGER_ENV, applied only when unset, plus the in-use flag.
    if std::env::var_os("LESS").is_none() {
        cmd.env("LESS", "FRX");
    }
    if std::env::var_os("LV").is_none() {
        cmd.env("LV", "-c");
    }
    cmd.env("GIT_PAGER_IN_USE", "true");

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        // Pager unavailable: run unpaged rather than fail the command, as git does.
        Err(_) => return,
    };

    // Also mark our own environment so in-process checks (e.g. column layout,
    // color auto-detection) treat output as a terminal, matching git.
    let hosted = crate::hosted::is_hosted();
    let prev_pager_in_use = hosted.then(|| std::env::var_os("GIT_PAGER_IN_USE"));
    std::env::set_var("GIT_PAGER_IN_USE", "true");

    let stdin = child.stdin.take().expect("stdin piped");
    let pipe_fd = stdin.as_raw_fd();

    // Flush anything already buffered on stdout before swapping the fd out.
    let _ = std::io::stdout().flush();

    // Park the host's descriptors before anything is dup2'd over them. They go
    // to fd >= 10 with CLOEXEC, which is the same discipline zsh applies to its
    // own internal fds (`movefd`): a shell script may well name fds 3-9 itself,
    // and a spare copy of stdout sitting on one of them would be visible to
    // `exec 3>&-`. Nothing to park when zvcs owns the process.
    let saved_stdout = hosted.then(|| park_fd(libc::STDOUT_FILENO)).flatten();

    let stderr_redirected;
    let saved_stderr;
    // SAFETY: raw fd dup/isatty on our own descriptors; single-threaded here
    // (called before dispatch spawns any worker).
    unsafe {
        libc::dup2(pipe_fd, libc::STDOUT_FILENO);
        stderr_redirected = libc::isatty(libc::STDERR_FILENO) == 1;
        saved_stderr = if stderr_redirected {
            let parked = hosted.then(|| park_fd(libc::STDERR_FILENO)).flatten();
            libc::dup2(pipe_fd, libc::STDERR_FILENO);
            parked
        } else {
            None
        };
    }
    // Drop the original pipe end: only fd 1 (and fd 2) now hold the write side,
    // so the pager sees EOF once `finish` closes them.
    drop(stdin);

    *PAGER.lock().unwrap() = Some(Pager {
        child,
        stderr_redirected,
        saved_stdout,
        saved_stderr,
        prev_pager_in_use,
    });
}

/// Duplicate `fd` onto a descriptor at 10 or above, close-on-exec.
///
/// `dup` hands back the lowest free number, which would put the parked copy in
/// the 3-9 range a shell script manipulates directly. `F_DUPFD_CLOEXEC` takes a
/// floor instead, and the CLOEXEC keeps the copy out of the pager child and out
/// of anything else spawned while it runs.
fn park_fd(fd: RawFd) -> Option<RawFd> {
    // SAFETY: fcntl on a descriptor we own; returns -1 on failure, never a
    // borrowed fd.
    let parked = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 10) };
    (parked >= 0).then_some(parked)
}

/// Is a pager child of ours installed over stdout?
///
/// Distinguishes the pager quitting early — a normal end to the session — from
/// an unrelated downstream reader closing the pipe, which git dies on. See
/// [`crate::sigpipe::exit_broken_pipe`].
pub fn is_active() -> bool {
    PAGER.lock().is_ok_and(|p| p.is_some())
}

/// Tear the pager down: flush our streams, close the redirected fds so the pager
/// reads EOF, then wait for it to exit. No-op when no pager was installed.
pub fn finish() {
    let Some(mut pager) = PAGER.lock().unwrap().take() else {
        return;
    };
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    // SAFETY: closing or restoring the fds we dup2'd in `spawn`. git's
    // `wait_for_pager` does the plain `close(1)` to signal end-of-input, and
    // that is still what happens when zvcs owns the process. Hosted, the parked
    // copy is dup2'd back instead: that closes the pipe end sitting on fd 1 —
    // so the pager still reads EOF — and hands the host its own stdout back in
    // one step, rather than leaving it with a closed descriptor.
    unsafe {
        match pager.saved_stdout {
            Some(saved) => {
                libc::dup2(saved, libc::STDOUT_FILENO);
                libc::close(saved);
            }
            None => {
                libc::close(libc::STDOUT_FILENO);
            }
        }
        if pager.stderr_redirected {
            match pager.saved_stderr {
                Some(saved) => {
                    libc::dup2(saved, libc::STDERR_FILENO);
                    libc::close(saved);
                }
                None => {
                    libc::close(libc::STDERR_FILENO);
                }
            }
        }
    }
    if let Some(prev) = pager.prev_pager_in_use {
        match prev {
            Some(value) => std::env::set_var("GIT_PAGER_IN_USE", value),
            None => std::env::remove_var("GIT_PAGER_IN_USE"),
        }
    }
    let _ = pager.child.wait();
}

/// An environment variable read as a git boolean flag (`true`/`1`/`yes`/`on`).
/// `pager_in_use()` (pager.c): whether an ancestor already set up a pager that
/// this process is writing into.
///
/// Read by more than the pager itself — git's `auto_decoration_style()` is
/// `isatty(1) || pager_in_use()`, so `git log`'s decorations appear when the
/// output is going to a pager even though stdout is a pipe.
pub fn in_use() -> bool {
    env_flag("GIT_PAGER_IN_USE")
}

fn env_flag(name: &str) -> bool {
    matches!(
        std::env::var(name).ok().as_deref(),
        Some("true" | "1" | "yes" | "on")
    )
}

/// An environment variable, treated as absent when empty (git ignores an empty
/// `$GIT_PAGER` / `$PAGER` and moves down the chain).
fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

#[cfg(test)]
mod hosted_fd_tests {
    use super::*;

    /// fd 1's identity: the file description behind it, which survives a `dup2`
    /// of a copy back onto it and does not survive a `close`.
    fn stdout_identity() -> Option<(u64, u64)> {
        // SAFETY: fstat on a descriptor we own, into a zeroed stat we then read.
        unsafe {
            let mut st: libc::stat = std::mem::zeroed();
            (libc::fstat(libc::STDOUT_FILENO, &mut st) == 0)
                .then_some((st.st_dev as u64, st.st_ino as u64))
        }
    }

    /// A hosted pager session gives the host back the stdout it started with.
    ///
    /// The regression: `finish` closed fd 1 outright — right for git, which
    /// exits next, fatal for a host, which does not. Inside zshrs-native every
    /// command after a paged `git log` wrote to a closed descriptor and the
    /// terminal stayed silent for the rest of the session.
    #[test]
    fn a_hosted_pager_session_leaves_stdout_where_it_found_it() {
        let before = stdout_identity().expect("fd 1 open at test start");
        let prior_flag = std::env::var_os("GIT_PAGER_IN_USE");

        // `cat` is a pager that needs no terminal and exits on EOF, so the
        // session completes without a human pressing `q`.
        crate::hosted::run(|| {
            spawn("cat");
            finish();
            0
        });

        assert_eq!(
            stdout_identity(),
            Some(before),
            "fd 1 must be the same file description the host had"
        );
        assert_eq!(
            std::env::var_os("GIT_PAGER_IN_USE"),
            prior_flag,
            "the in-use flag must not outlive the pager"
        );
    }
}

#[cfg(test)]
mod term_columns_tests {
    use super::atoi;

    /// C's `atoi`, which is what `term_columns()` runs on `$COLUMNS`. The `+`
    /// sign and the embedded-junk cases are the ones the three former copies
    /// disagreed on.
    #[test]
    fn atoi_is_c_atoi() {
        assert_eq!(atoi(b"100"), Some(100));
        assert_eq!(atoi(b"+100"), Some(100));
        assert_eq!(atoi(b"-5"), Some(-5));
        assert_eq!(atoi(b"  96  "), Some(96));
        assert_eq!(atoi(b"1-2"), Some(1));
        assert_eq!(atoi(b"120junk"), Some(120));
        assert_eq!(atoi(b"0"), Some(0));
        assert_eq!(atoi(b"abc"), None);
        assert_eq!(atoi(b""), None);
        assert_eq!(atoi(b"+"), None);
    }
}
