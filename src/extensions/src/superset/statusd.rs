//! Background status maintainer — a persistent worker pool that NEVER idles.
//!
//! `repo_status` must be warm and fresh for `zdashboard` / `zstatus --all` to be
//! instant *and* accurate, and a live dirtiness scan over thousands of repos is
//! too slow to do on demand. So the daemon runs a dedicated pool — one worker per
//! core — that perpetually sweeps the index: each worker pulls the next repo off a
//! shared rotating cursor, recomputes its status, writes it, and immediately grabs
//! the next, forever. There is no pause; the pool is always working, so every
//! repo's status is refreshed every few seconds. The reactive watch still updates
//! a repo the instant it changes; this keeps the whole index continually fresh.
//!
//! The sweep is a single always-running "status maintainer" ledger job whose
//! output reports live throughput, so `zjobs` / `zjob` show it working. Disable
//! the whole thing with `zvcs.statusinterval = 0`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;

/// One repo to keep fresh: its ledger id and git dir.
struct Target {
    id: i64,
    git_dir: PathBuf,
}

/// Spawn the maintainer pool on the daemon, unless disabled.
pub fn spawn_if_enabled() {
    if interval_secs() == 0 {
        return; // `zvcs.statusinterval = 0` turns the maintainer off.
    }
    let workers = thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let cursor = Arc::new(AtomicUsize::new(0));
    let updates = Arc::new(AtomicU64::new(0));
    let snapshot: Arc<RwLock<Arc<Vec<Target>>>> = Arc::new(RwLock::new(Arc::new(load_targets())));
    let job = create_job();

    // The workers: each owns a db connection and perpetually claims the next repo
    // off the shared cursor, computes its status, and writes it.
    for _ in 0..workers {
        let cursor = Arc::clone(&cursor);
        let updates = Arc::clone(&updates);
        let snapshot = Arc::clone(&snapshot);
        thread::spawn(move || {
            let mut conn = crate::db::open_rw().ok();
            loop {
                if conn.is_none() {
                    thread::sleep(Duration::from_secs(1));
                    conn = crate::db::open_rw().ok();
                    continue;
                }
                let repos = { snapshot.read().unwrap().clone() };
                if repos.is_empty() {
                    thread::sleep(Duration::from_millis(500));
                    continue;
                }
                let target = &repos[cursor.fetch_add(1, Ordering::Relaxed) % repos.len()];
                if let Ok(repo) = gix::open(&target.git_dir) {
                    let (dirty, detached, sync, head) = crate::superset::status::compute(&repo);
                    if let Some(c) = &conn {
                        // A write error (e.g. a dropped connection) → reopen next loop.
                        if crate::db::upsert_status(c, target.id, dirty, detached, &sync, &head).is_err() {
                            conn = None;
                        }
                    }
                }
                updates.fetch_add(1, Ordering::Relaxed);
            }
        });
    }

    // The coordinator: refresh the repo snapshot (so newly-indexed repos join and
    // deleted ones drop out) and publish live throughput to the maintainer job.
    thread::spawn(move || {
        let mut last = 0u64;
        loop {
            thread::sleep(Duration::from_secs(5));
            let fresh = load_targets();
            let indexed = fresh.len();
            *snapshot.write().unwrap() = Arc::new(fresh);
            let total = updates.load(Ordering::Relaxed);
            let rate = total.saturating_sub(last) / 5;
            last = total;
            publish(job, workers, indexed, total, rate);
        }
    });
}

/// Load every indexed repo as a [`Target`].
fn load_targets() -> Vec<Target> {
    let Ok(conn) = crate::db::open_ro() else { return Vec::new() };
    let Ok(repos) = crate::db::list_repos(&conn) else { return Vec::new() };
    repos.into_iter().map(|r| Target { id: r.id, git_dir: PathBuf::from(r.git_dir) }).collect()
}

/// Insert the always-running "status maintainer" ledger row, returning its id.
fn create_job() -> Option<i64> {
    let conn = crate::db::open_rw().ok()?;
    let id = crate::db::insert_job(&conn, None, "status maintainer", "{\"kind\":\"statusd\"}", None).ok()?;
    let _ = crate::db::job_running(&conn, id);
    Some(id)
}

/// Update the maintainer job's live progress line (kept in the `running` state).
fn publish(job: Option<i64>, workers: usize, indexed: usize, updates: u64, rate: u64) {
    let (Some(id), Ok(conn)) = (job, crate::db::open_rw()) else {
        return;
    };
    let out = format!("{workers} workers · {indexed} repos indexed · {updates} status writes · ~{rate}/s");
    let _ = conn.execute(
        "UPDATE jobs SET state='running', output=?2 WHERE id=?1",
        rusqlite::params![id, out],
    );
}

/// `zvcs.statusinterval`: any non-zero value (or unset) enables the continuous
/// maintainer; `0` disables it. Default enabled.
fn interval_secs() -> u64 {
    gix::discover(".")
        .ok()
        .and_then(|r| r.config_snapshot().integer("zvcs.statusinterval"))
        .filter(|n| *n >= 0)
        .map(|n| n as u64)
        .unwrap_or(10)
}
