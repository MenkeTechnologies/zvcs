//! `git zppid` — per-process commit productivity, plus the commit-tracking hook
//! the dispatcher fires after every commit-producing verb.
//!
//! The fleet runs many processes that drive git concurrently — one per agent /
//! login. The naive identity, `getppid()`, is useless: a `git commit` is run by a
//! throwaway shell (an agent spawns a fresh `zsh -c …` per command, or a script
//! subshells), so the parent pid is different on every commit. Keying on it yields
//! a flood of one-commit rows, never a stable "N agents" view.
//!
//! So instead of the immediate parent we find the **responsible process**: walk up
//! the parent chain, skip transient wrapper shells (a shell invoked with `-c`, which
//! runs one command and dies), and stop at the first durable process — a real
//! program (an agent, editor, daemon) or an interactive login shell. That pid is
//! stable across every commit one agent makes, so N concurrent agents map to N rows.
//! Each row also carries that process's command name and cwd, so the pid is legible.
//!
//! Attribution happens at command time: [`note_commit`] is called by
//! [`crate::dispatch::run`] right after any [commit-producing verb](COMMIT_VERBS)
//! returns, and credits the responsible process only when HEAD actually advanced —
//! a no-op `commit` (nothing staged) or a rejected merge counts nothing.

use anyhow::Result;
use std::process::ExitCode;

/// Verbs that can add a commit to the current branch. A successful invocation of
/// one of these that moves HEAD to a new tip is credited to the responsible process.
/// `rebase`/`merge` may move HEAD without authoring the commit(s); that is counted
/// as one landed operation, not one per replayed commit — see the module docs.
const COMMIT_VERBS: &[&str] = &["commit", "commit-tree", "merge", "cherry-pick", "revert", "am", "rebase"];

/// Whether `sub` is a commit-producing verb worth probing HEAD around.
pub fn is_commit_verb(sub: &str) -> bool {
    COMMIT_VERBS.contains(&sub)
}

/// The current repo's peeled HEAD commit id, or `None` outside a repo / on an
/// unborn branch. The cheap before/after probe [`note_commit`] compares. Any error
/// (not a repo, detached read failure) collapses to `None` — a `None → Some`
/// transition is the first commit on an unborn branch and still counts.
pub fn head_commit() -> Option<gix::ObjectId> {
    let repo = gix::discover(".").ok()?;
    repo.head().ok()?.try_peel_to_id().ok().flatten().map(|id| id.detach())
}

/// Whether `argv0` names a shell (by basename, ignoring a login shell's leading `-`).
fn is_shell(argv0: &str) -> bool {
    let base = argv0.rsplit('/').next().unwrap_or(argv0).trim_start_matches('-');
    matches!(base, "sh" | "bash" | "zsh" | "dash" | "fish" | "ksh" | "tcsh" | "csh")
}

/// A short command name from an argv0 path: the basename, minus a login shell's `-`.
fn cmd_name(argv0: &str) -> String {
    argv0.rsplit('/').next().unwrap_or(argv0).trim_start_matches('-').to_string()
}

/// `(ppid, argv0, is_wrapper)` for `pid` via one `ps`. `is_wrapper` is true for a
/// shell invoked with `-c` — a transient per-command wrapper to climb past. `None`
/// when the pid is gone or unreadable.
fn proc_row(pid: i64) -> Option<(i64, String, bool)> {
    let out = std::process::Command::new("ps")
        .args(["-o", "ppid=,args=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&out.stdout);
    let mut it = line.split_whitespace();
    let ppid: i64 = it.next()?.parse().ok()?;
    let argv0 = it.next()?.to_string();
    let is_wrapper = is_shell(&argv0) && it.any(|a| a == "-c");
    Some((ppid, argv0, is_wrapper))
}

/// A process's current working directory. Linux reads `/proc/<pid>/cwd` directly (no
/// fork); macOS/BSD fall back to `lsof`'s cwd fd. Empty when it can't be read.
fn proc_cwd(pid: i64) -> String {
    if let Ok(p) = std::fs::read_link(format!("/proc/{pid}/cwd")) {
        return p.to_string_lossy().into_owned();
    }
    let Ok(out) = std::process::Command::new("lsof")
        .args(["-a", "-p", &pid.to_string(), "-d", "cwd", "-Fn"])
        .output()
    else {
        return String::new();
    };
    // -Fn: newline-separated fields, the cwd path on an `n`-prefixed line.
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix('n').map(|s| s.to_string()))
        .unwrap_or_default()
}

/// The durable process responsible for this git command: `(pid, cmd, cwd)`. Walks
/// up from `getppid()`, skipping transient `-c` wrapper shells, and stops at the
/// first durable process (a real program or an interactive shell). Bounded so a
/// pid-reuse cycle can never spin.
fn responsible_process() -> (i64, String, String) {
    let mut pid = std::os::unix::process::parent_id() as i64;
    let (mut chosen, mut cmd) = (pid, String::new());
    for _ in 0..32 {
        let Some((ppid, argv0, is_wrapper)) = proc_row(pid) else { break };
        if is_wrapper && ppid > 1 {
            pid = ppid; // transient `sh -c` wrapper → keep climbing
            continue;
        }
        chosen = pid;
        cmd = cmd_name(&argv0);
        break;
    }
    (chosen, cmd, proc_cwd(chosen))
}

/// Credit the responsible process with one commit iff HEAD advanced from `before` to
/// a new, non-null tip. Best-effort: swallows every error so it can never fail or
/// slow the command it follows. Opens the rw db only on an actual advance.
pub fn note_commit(before: Option<gix::ObjectId>) {
    let after = head_commit();
    if after.is_none() || after == before {
        return; // no new commit landed
    }
    let (pid, cmd, cwd) = responsible_process();
    // An explicit `ZVCS_SESSION` still overrides the identity; otherwise the durable
    // responsible-process pid is the key, so one agent is one stable row.
    let session = std::env::var("ZVCS_SESSION")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("pid-{pid}"));
    if let Ok(conn) = crate::db::open_rw() {
        let _ = crate::db::ppid_record_commit(&conn, &session, pid, &cmd, &cwd);
    }
}

/// Whether `pid` is still a live process. `kill(pid, 0)` succeeds for a live,
/// signalable process; `EPERM` means it exists but is owned by another uid (still
/// alive); `ESRCH` (and anything else) means gone. Uses `last_os_error` rather than
/// a raw `errno` symbol so it builds identically on macOS and Linux. Shared with the
/// dashboard's process tile.
pub fn is_alive(pid: i64) -> bool {
    if pid <= 0 {
        return false;
    }
    if unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Replace a leading `$HOME` with `~` for compact display.
pub fn tilde(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() && path.starts_with(&h) => format!("~{}", &path[h.len()..]),
        _ => path.to_string(),
    }
}

/// `git zppid [--json]` — list every tracked process, most commits first.
pub fn zppid(args: &[String]) -> Result<ExitCode> {
    let json = args.iter().any(|a| a == "--json");

    let Ok(conn) = crate::db::open_ro() else {
        if json {
            println!("[]");
        } else {
            println!("no processes recorded yet");
        }
        return Ok(ExitCode::SUCCESS);
    };
    let procs = crate::db::list_ppids(&conn)?;
    let now = crate::date::now_seconds();

    if json {
        let arr: Vec<_> = procs
            .iter()
            .map(|p| {
                serde_json::json!({
                    "pid": p.ppid,
                    "cmd": p.cmd,
                    "cwd": p.cwd,
                    "alive": is_alive(p.ppid),
                    "commits": p.commits,
                    "first_seen": p.first_seen,
                    "last_seen": p.last_seen,
                })
            })
            .collect();
        println!("{}", serde_json::json!(arr));
        return Ok(ExitCode::SUCCESS);
    }

    if procs.is_empty() {
        println!("no processes recorded yet");
        return Ok(ExitCode::SUCCESS);
    }

    let total: i64 = procs.iter().map(|p| p.commits).sum();
    let live = procs.iter().filter(|p| is_alive(p.ppid)).count();
    println!("{} process(es), {live} live, {total} commit(s):", procs.len());
    println!("  {:>7} {:>5} {:>7} {:>5}  {:<16} {}", "PID", "STATE", "COMMITS", "LAST", "CMD", "CWD");
    for p in &procs {
        let state = if is_alive(p.ppid) { "live" } else { "dead" };
        println!(
            "  {:>7} {:>5} {:>7} {:>5}  {:<16} {}",
            p.ppid,
            state,
            p.commits,
            ago((now - p.last_seen).max(0)),
            trunc(&p.cmd, 16),
            tilde(&p.cwd),
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// Compact "time ago" for a fixed-width column: `s`/`m`/`h`/`d`.
fn ago(secs: i64) -> String {
    let s = secs.max(0);
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86_400 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d", s / 86_400)
    }
}

fn trunc(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max.saturating_sub(1)).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_verbs_recognized() {
        assert!(is_commit_verb("commit"));
        assert!(is_commit_verb("merge"));
        assert!(is_commit_verb("cherry-pick"));
        assert!(!is_commit_verb("status"));
        assert!(!is_commit_verb("push"));
    }

    #[test]
    fn shell_and_cmd_name() {
        assert!(is_shell("/opt/homebrew/bin/zsh"));
        assert!(is_shell("-zsh")); // login shell
        assert!(is_shell("bash"));
        assert!(!is_shell("claude"));
        assert!(!is_shell("/usr/bin/node"));
        assert_eq!(cmd_name("/opt/homebrew/bin/zsh"), "zsh");
        assert_eq!(cmd_name("-zsh"), "zsh");
        assert_eq!(cmd_name("claude"), "claude");
    }

    #[test]
    fn responsible_process_finds_a_live_ancestor() {
        // From the test process the walk must resolve to a real, live pid (whatever
        // ran the test harness), never 0.
        let (pid, _cmd, _cwd) = responsible_process();
        assert!(pid > 1);
        assert!(is_alive(pid));
    }

    #[test]
    fn alive_current_process_true_and_bogus_false() {
        assert!(is_alive(std::process::id() as i64));
        assert!(!is_alive(0));
        assert!(!is_alive(-5));
    }

    #[test]
    fn trunc_shortens_with_ellipsis() {
        assert_eq!(trunc("short", 22), "short");
        assert_eq!(trunc("aaaaa", 3), "aa…");
    }
}
