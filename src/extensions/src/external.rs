//! Git's dashed-external-command dispatch (`git foo` → exec `git-foo` from PATH).
//!
//! Faithful port of git.c's `execv_dashed_external`: once a verb proves to be
//! neither a builtin nor an alias, git looks for a `git-<verb>` executable on
//! PATH and execs it before giving up via `help_unknown_cmd`. This is the
//! mechanism every third-party subcommand relies on — `git fuzzy`, `git lfs`,
//! `git flow`, `git absorb`, `git town`, … — so shadowing stock `git` without it
//! silently breaks them all (git-fuzzy calls `git fuzzy helper` on every keystroke
//! and preview, recursing through whichever `git` is on PATH).
//!
//! We exec (replace this process) rather than spawn+wait, matching git: the
//! external owns the terminal outright — which a full-screen fzf TUI needs — and
//! its signals and exit status flow straight through with no intermediary.

use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode};

/// Try to run `git-<cmd> <args>` from PATH. Returns:
///   * never (this process is replaced) when the external exists and execs;
///   * `Some(FAILURE)` when it exists but cannot be executed;
///   * `None` when no such external is on PATH — the caller then falls through to
///     autocorrect / "not a git command", exactly as git's `help_unknown_cmd`
///     does after `execv_dashed_external` fails with `ENOENT`.
pub fn try_dashed(cmd: &str, args: &[String]) -> Option<ExitCode> {
    let exe = format!("git-{cmd}");
    // `Command` PATH-searches a slash-free program name (execvp semantics), so a
    // bare `git-<cmd>` resolves against PATH just as git's own lookup does.
    let err = Command::new(&exe).args(args).exec();
    // `exec` returns only on failure. A missing external is the ordinary case
    // (the verb was simply a typo) — stay silent and let the caller diagnose it.
    if err.kind() == std::io::ErrorKind::NotFound {
        return None;
    }
    // It exists but is not runnable (not executable, bad interpreter, …) — git
    // reports this rather than pretending the command is unknown.
    eprintln!("zvcs: cannot exec '{exe}': {err}");
    Some(ExitCode::FAILURE)
}
