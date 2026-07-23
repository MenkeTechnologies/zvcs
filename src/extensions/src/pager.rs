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
use std::os::unix::io::AsRawFd;
use std::process::{Child, Command, Stdio};
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
}

static PAGER: Mutex<Option<Pager>> = Mutex::new(None);

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
    let repo = gix::discover(".").ok();
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
fn resolve_pager(cfg: Option<&gix::config::Snapshot<'_>>) -> String {
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

/// Spawn the pager through the shell (so `core.pager = "less -S"` and pipelines
/// work) and redirect our stdout — and stderr when it is a tty — onto it.
fn spawn(program: &str) {
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(program).stdin(Stdio::piped());
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
    std::env::set_var("GIT_PAGER_IN_USE", "true");

    let stdin = child.stdin.take().expect("stdin piped");
    let pipe_fd = stdin.as_raw_fd();

    // Flush anything already buffered on stdout before swapping the fd out.
    let _ = std::io::stdout().flush();

    let stderr_redirected;
    // SAFETY: raw fd dup/isatty on our own descriptors; single-threaded here
    // (called before dispatch spawns any worker).
    unsafe {
        libc::dup2(pipe_fd, libc::STDOUT_FILENO);
        stderr_redirected = libc::isatty(libc::STDERR_FILENO) == 1;
        if stderr_redirected {
            libc::dup2(pipe_fd, libc::STDERR_FILENO);
        }
    }
    // Drop the original pipe end: only fd 1 (and fd 2) now hold the write side,
    // so the pager sees EOF once `finish` closes them.
    drop(stdin);

    *PAGER.lock().unwrap() = Some(Pager {
        child,
        stderr_redirected,
    });
}

/// Tear the pager down: flush our streams, close the redirected fds so the pager
/// reads EOF, then wait for it to exit. No-op when no pager was installed.
pub fn finish() {
    let Some(mut pager) = PAGER.lock().unwrap().take() else {
        return;
    };
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    // SAFETY: closing the fds we dup2'd in `spawn`; git's `wait_for_pager` does
    // the same `close(1)` to signal end-of-input to the pager.
    unsafe {
        libc::close(libc::STDOUT_FILENO);
        if pager.stderr_redirected {
            libc::close(libc::STDERR_FILENO);
        }
    }
    let _ = pager.child.wait();
}

/// An environment variable read as a git boolean flag (`true`/`1`/`yes`/`on`).
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
