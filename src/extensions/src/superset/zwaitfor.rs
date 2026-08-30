//! `git zwaitfor <condition> [--timeout <secs>]` — block until a tree-wide state
//! holds, then exit 0 (1 on timeout). A cross-repo barrier on STATE, where
//! `zbarrier`/`zwait` are job-scoped. Conditions (from the daemon's cached
//! `repo_status`, so the daemon must be maintaining status):
//!
//!   clean            every indexed repo is clean (nothing uncommitted)
//!   idle             no queued or running daemon jobs
//!   synced           every repo is up-to-date with its upstream
//!   <substr> <sha>   the repo whose path contains <substr> is at <sha> (prefix)

use anyhow::{anyhow, Result};
use std::process::ExitCode;
use std::time::{Duration, Instant};

pub fn zwaitfor(args: &[String]) -> Result<ExitCode> {
    let mut timeout = 60u64;
    let mut pos: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--timeout" | "-t" => {
                timeout = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(timeout);
                i += 2;
            }
            _ => {
                pos.push(args[i].clone());
                i += 1;
            }
        }
    }
    if pos.is_empty() {
        return Err(anyhow!(
            "usage: git zwaitfor <clean|idle|synced|<repo> <sha>> [--timeout <secs>]"
        ));
    }
    let deadline = Instant::now() + Duration::from_secs(timeout);
    loop {
        if check(&pos)? {
            return Ok(ExitCode::SUCCESS);
        }
        if Instant::now() >= deadline {
            eprintln!("zvcs: zwaitfor: timed out after {timeout}s waiting for `{}`", pos.join(" "));
            return Ok(ExitCode::from(1));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn check(pos: &[String]) -> Result<bool> {
    // The `<repo> <sha>` form needs a sha — validate it before touching the db so
    // a usage error is reported rather than swallowed by the poll loop.
    let want_sha = if matches!(pos[0].as_str(), "clean" | "idle" | "synced") {
        None
    } else {
        Some(pos.get(1).ok_or_else(|| anyhow!("usage: git zwaitfor <repo-substr> <sha>"))?)
    };
    // Read-only: a state barrier must not fail because the daemon holds the write
    // lock, and a not-yet-created db just means the condition isn't observable —
    // treat both as "not met yet" and keep polling.
    let Ok(conn) = crate::db::open_ro() else { return Ok(false) };
    let met = match pos[0].as_str() {
        "clean" => tree_reported(&conn, |(_, dirty, _, _)| !*dirty)?,
        "idle" => crate::db::contention(&conn)?.is_empty(),
        "synced" => tree_reported(&conn, |(_, _, sync, _)| is_synced(sync))?,
        repo => {
            let sha = want_sha.unwrap();
            crate::db::all_status(&conn)?
                .iter()
                .any(|(path, _, _, head)| path.contains(repo) && head.starts_with(sha))
        }
    };
    Ok(met)
}

/// Does every *indexed* repository have a cached status, and does `holds` hold
/// for all of them?
///
/// The condition used to be `all_status(..).iter().all(..)`, and `all()` over an
/// empty iterator is true: on a machine where nothing maintains the status cache
/// — no daemon, or one that has not reached these repositories yet — `git
/// zwaitfor clean` returned success immediately and reported the whole tree
/// clean while knowing nothing about it. A barrier that cannot observe its
/// condition must not claim the condition holds; it waits, and the timeout is
/// what tells the caller nothing is reporting.
///
/// The man page states the condition as "every indexed repo", so an indexed
/// repository with no status row is a repository not yet reported on, not one
/// that passes by absence. An empty index is unobservable for the same reason
/// and is likewise not met.
fn tree_reported(
    conn: &rusqlite::Connection,
    holds: impl Fn(&(String, bool, String, String)) -> bool,
) -> Result<bool> {
    let indexed = crate::db::list_repos(conn)?;
    if indexed.is_empty() {
        return Ok(false);
    }
    let status = crate::db::all_status(conn)?;
    let reported: std::collections::HashSet<&str> = status.iter().map(|(p, ..)| p.as_str()).collect();
    let every_repo_reported = indexed.iter().all(|r| {
        let path = r.workdir.as_deref().unwrap_or(r.git_dir.as_str());
        reported.contains(path)
    });
    Ok(every_repo_reported && status.iter().all(holds))
}

/// A repo is "in sync" if it has nothing to push or pull — up-to-date, or simply
/// no upstream to compare against (local-only, unborn). Only ahead / behind /
/// diverged / unrelated count as out-of-sync, so `synced` isn't held hostage by a
/// single local-only repo in the tree.
fn is_synced(sync: &str) -> bool {
    matches!(sync, "" | "up-to-date" | "no-upstream" | "unborn")
}

#[cfg(test)]
mod tests {
    use super::is_synced;

    #[test]
    fn synced_ignores_repos_with_nothing_to_sync() {
        // Nothing to push/pull → in sync (the bug: no-upstream wrongly blocked it).
        for s in ["", "up-to-date", "no-upstream", "unborn"] {
            assert!(is_synced(s), "`{s}` should count as synced");
        }
        // Genuinely out of sync.
        for s in ["ahead", "behind", "diverged", "unrelated"] {
            assert!(!is_synced(s), "`{s}` should NOT count as synced");
        }
    }
}
