//! Coordination verbs over the async job queue — `zqueue`, `zwait`, `zbarrier`.
//!
//! The daemon runs `zcommit`/`zpush` jobs asynchronously and records them in the
//! ledger (`queued` → `running` → a terminal state). These verbs read that
//! ledger: `zqueue` shows what is in flight, `zwait` blocks until one repo's jobs
//! drain, and `zbarrier` blocks until the whole queue is idle — the join half of
//! the fire-and-forget async model. With no daemon (hence no jobs) they are
//! immediate no-ops.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};

/// Ledger states that are still in flight.
fn is_active(state: &str) -> bool {
    state == "queued" || state == "running"
}

/// `git zqueue` — list the async jobs that are queued or running.
pub fn zqueue(_args: &[String]) -> Result<ExitCode> {
    let Ok(conn) = crate::db::open_ro() else {
        println!("queue empty");
        return Ok(ExitCode::SUCCESS);
    };
    let jobs = crate::db::list_jobs(&conn, 1000)?;
    let active: Vec<_> = jobs.iter().filter(|j| is_active(&j.state)).collect();
    if active.is_empty() {
        println!("queue empty");
        return Ok(ExitCode::SUCCESS);
    }
    for j in &active {
        let repo = j.git_dir.as_deref().unwrap_or("?");
        println!("#{:<5} {:<8} {:<10} {}", j.id, j.state, j.kind, repo);
    }
    eprintln!("zqueue: {} active", active.len());
    Ok(ExitCode::SUCCESS)
}

/// Count active jobs, optionally only those for `git_dir`.
fn active_count(filter: Option<&Path>) -> usize {
    let Ok(conn) = crate::db::open_ro() else { return 0 };
    let Ok(jobs) = crate::db::list_jobs(&conn, 1000) else { return 0 };
    jobs.iter()
        .filter(|j| is_active(&j.state))
        .filter(|j| match filter {
            None => true,
            Some(want) => j
                .git_dir
                .as_deref()
                .and_then(|g| Path::new(g).canonicalize().ok())
                .map(|g| g == want)
                .unwrap_or(false),
        })
        .count()
}

/// Block until `filter`'s active jobs reach zero, or the timeout elapses.
fn wait_until_idle(filter: Option<PathBuf>, label: &str) -> Result<ExitCode> {
    const TIMEOUT: Duration = Duration::from_secs(300);
    let start = Instant::now();
    loop {
        let n = active_count(filter.as_deref());
        if n == 0 {
            println!("{label}: idle");
            return Ok(ExitCode::SUCCESS);
        }
        if start.elapsed() >= TIMEOUT {
            eprintln!("{label}: still {n} active after {}s", TIMEOUT.as_secs());
            return Ok(ExitCode::FAILURE);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// `git zwait [<path>]` — block until the repo at `<path>` (or cwd) has no queued
/// or running async jobs left.
pub fn zwait(args: &[String]) -> Result<ExitCode> {
    let path = args.iter().find(|a| !a.starts_with('-')).cloned().unwrap_or_else(|| ".".to_string());
    let Ok(repo) = gix::discover(&path) else {
        bail!("not a git repository: {path}");
    };
    let Ok(git_dir) = repo.git_dir().canonicalize() else {
        bail!("cannot resolve git dir for {path}");
    };
    wait_until_idle(Some(git_dir), "zwait")
}

/// `git zbarrier` — block until the entire async queue is idle (every repo's jobs
/// have drained), the join point after a burst of `zcommit`/`zpush`.
pub fn zbarrier(_args: &[String]) -> Result<ExitCode> {
    wait_until_idle(None, "zbarrier")
}
