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
        "clean" => crate::db::all_status(&conn)?.iter().all(|(_, dirty, _, _)| !dirty),
        "idle" => crate::db::contention(&conn)?.is_empty(),
        "synced" => crate::db::all_status(&conn)?.iter().all(|(_, _, sync, _)| is_synced(sync)),
        repo => {
            let sha = want_sha.unwrap();
            crate::db::all_status(&conn)?
                .iter()
                .any(|(path, _, _, head)| path.contains(repo) && head.starts_with(sha))
        }
    };
    Ok(met)
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
