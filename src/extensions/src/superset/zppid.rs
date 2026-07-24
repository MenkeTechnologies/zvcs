//! `git zppid` — per-process commit productivity, plus the commit-tracking hook
//! the dispatcher fires after every commit-producing verb.
//!
//! Many processes drive git concurrently across the fleet — any shell, agent, or
//! program, one per tmux pane / login. Each is identified by [`crate::session_key`]
//! (an exported `ZVCS_SESSION` or the `pid-<ppid>` fallback), and this module keeps
//! one persistent row per process in the `ppids` table, accumulating a commit tally
//! so you can see which processes are actually landing work. The stored id is the
//! **ppid** — the git process's parent, i.e. the invoking shell/agent/program.
//!
//! Attribution happens at command time, not in the daemon: [`note_commit`] is
//! called by [`crate::dispatch::run`] right after any [commit-producing verb](COMMIT_VERBS)
//! returns. It compares HEAD before/after and credits one commit to the running
//! process whenever HEAD advanced to a new tip — so the count reflects commits that
//! actually landed, and a no-op `commit` (nothing staged) or a rejected merge adds
//! nothing. Because it keys on the invoking process, the commit is attributed to
//! the process that ran it, which a daemon-side HEAD-delta observer could not do.

use anyhow::Result;
use std::process::ExitCode;

/// Verbs that can add a commit to the current branch. A successful invocation of
/// one of these that moves HEAD to a new tip is credited to the running process.
/// `rebase`/`merge` may move HEAD without authoring the commit(s); that is counted
/// as one landed operation, not one per replayed commit — see the module docs.
/// Kept deliberately small: only verbs whose normal effect is a new HEAD.
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

/// Credit the running process with one commit iff HEAD advanced from `before` to a
/// new, non-null tip. Best-effort: swallows every error so it can never fail or slow
/// the command it follows. Opens the rw db (creating the ledger if absent) only on
/// an actual advance, so nothing is written for commands that did not commit.
pub fn note_commit(before: Option<gix::ObjectId>) {
    let after = head_commit();
    if after.is_none() || after == before {
        return; // no new commit landed
    }
    let session = crate::session_key();
    // The git process's parent — the shell/agent/program that ran this command.
    let ppid = std::os::unix::process::parent_id() as i64;
    if let Ok(conn) = crate::db::open_rw() {
        let _ = crate::db::ppid_record_commit(&conn, &session, ppid);
    }
}

/// Whether `ppid` is still a live process. `kill(ppid, 0)` succeeds for a live,
/// signalable process; `EPERM` means it exists but is owned by another uid (still
/// alive); `ESRCH` (and anything else) means gone. Uses `last_os_error` rather than
/// a raw `errno` symbol so it builds identically on macOS and Linux.
fn alive(ppid: i64) -> bool {
    if ppid <= 0 {
        return false;
    }
    if unsafe { libc::kill(ppid as libc::pid_t, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn trunc(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max.saturating_sub(1)).collect::<String>())
    }
}

/// `git zppid [--json]` — list every tracked process, most commits first.
pub fn zppid(args: &[String]) -> Result<ExitCode> {
    let json = args.iter().any(|a| a == "--json");

    // No ledger yet means nothing has committed through zvcs — an empty, not-an-error
    // result, so a fresh checkout answers cleanly instead of failing to open.
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
                    "session": p.session,
                    "ppid": p.ppid,
                    "alive": alive(p.ppid),
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
    let live = procs.iter().filter(|p| alive(p.ppid)).count();
    println!("{} process(es), {live} live, {total} commit(s):", procs.len());
    println!("  {:<22} {:>7} {:>5} {:>7}  {}", "SESSION", "PPID", "STATE", "COMMITS", "LAST");
    for p in &procs {
        let state = if alive(p.ppid) { "live" } else { "dead" };
        let last = crate::date::show_date_relative(p.last_seen, now);
        println!("  {:<22} {:>7} {:>5} {:>7}  {last}", trunc(&p.session, 22), p.ppid, state, p.commits);
    }
    Ok(ExitCode::SUCCESS)
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
    fn alive_current_process_true_and_bogus_false() {
        // Our own pid is trivially live; pid 0 and a negative pid are not.
        assert!(alive(std::process::id() as i64));
        assert!(!alive(0));
        assert!(!alive(-5));
    }

    #[test]
    fn trunc_shortens_with_ellipsis() {
        assert_eq!(trunc("short", 22), "short");
        assert_eq!(trunc("aaaaa", 3), "aa…");
    }
}
