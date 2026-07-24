//! Background status maintainer — the daemon's worker pool keeps `repo_status`
//! warm for EVERY indexed repo, so `zdashboard` and `zstatus --all` are instant
//! *and* accurate without needing `[zvcs] autostatus`.
//!
//! A full working-tree dirtiness scan over the whole index is expensive (seconds
//! across thousands of repos), so it runs on its own thread: one parallel pass
//! over all indexed repos, then a pause, repeating forever. The reactive watch
//! (when configured) still updates actively-changing repos the instant they
//! change; this thread fills in the cold ones and continually refreshes the rest.
//! Tunable via `zvcs.statusinterval` (seconds between passes; `0` disables).

use std::path::PathBuf;
use std::thread;
use std::time::Duration;

/// Spawn the maintainer on the daemon, unless `zvcs.statusinterval = 0`.
pub fn spawn_if_enabled() {
    let secs = interval_secs();
    if secs == 0 {
        return; // explicitly disabled
    }
    let pause = Duration::from_secs(secs);
    thread::spawn(move || loop {
        let n = refresh_all();
        // Daemon stdout is routed to ~/.zvcs/zvcs.log, never the terminal.
        println!("[zvcs statusd] refreshed status for {n} repo(s)");
        thread::sleep(pause);
    });
}

/// Seconds between full passes: `zvcs.statusinterval`, default 10.
fn interval_secs() -> u64 {
    gix::discover(".")
        .ok()
        .and_then(|r| r.config_snapshot().integer("zvcs.statusinterval"))
        .filter(|n| *n >= 0)
        .map(|n| n as u64)
        .unwrap_or(10)
}

/// Recompute and cache status for every indexed repo, in parallel across the
/// worker pool. Returns how many repos were written. The compute pass takes no
/// db lock; only the final batched write does, and briefly.
fn refresh_all() -> usize {
    let repos = match crate::db::open_ro().and_then(|c| crate::db::list_repos(&c)) {
        Ok(r) => r,
        Err(_) => return 0,
    };
    if repos.is_empty() {
        return 0;
    }
    let targets: Vec<(PathBuf, PathBuf)> = repos
        .iter()
        .map(|r| {
            let git_dir = PathBuf::from(&r.git_dir);
            let workdir = r.workdir.clone().map(PathBuf::from).unwrap_or_else(|| git_dir.clone());
            (git_dir, workdir)
        })
        .collect();

    // Parallel, read-only status computation across the machine's cores.
    let computed = crate::superset::query::parallel_map(&targets, |git_dir, _wd| {
        gix::open(git_dir).ok().map(|repo| crate::superset::status::compute(&repo))
    });

    // Write every result in one transaction, keyed by repo id (no side effects on
    // the repos table the crawler owns).
    let Ok(conn) = crate::db::open_rw() else { return 0 };
    let _ = conn.execute_batch("BEGIN");
    let mut written = 0usize;
    for (repo, status) in repos.iter().zip(&computed) {
        if let Some((dirty, detached, sync, head)) = status {
            if crate::db::upsert_status(&conn, repo.id, *dirty, *detached, sync, head).is_ok() {
                written += 1;
            }
        }
    }
    let _ = conn.execute_batch("COMMIT");
    written
}
