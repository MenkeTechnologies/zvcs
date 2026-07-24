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
    let conn = crate::db::open_rw()?;
    match pos[0].as_str() {
        "clean" => Ok(crate::db::all_status(&conn)?.iter().all(|(_, dirty, _, _)| !dirty)),
        "idle" => Ok(crate::db::contention(&conn)?.is_empty()),
        "synced" => Ok(crate::db::all_status(&conn)?
            .iter()
            .all(|(_, _, sync, _)| sync == "up-to-date" || sync.is_empty())),
        repo => {
            let sha = pos
                .get(1)
                .ok_or_else(|| anyhow!("usage: git zwaitfor <repo-substr> <sha>"))?;
            Ok(crate::db::all_status(&conn)?
                .iter()
                .any(|(path, _, _, head)| path.contains(repo) && head.starts_with(sha)))
        }
    }
}
