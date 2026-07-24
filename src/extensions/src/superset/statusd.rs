//! Background status maintainer — a persistent pool that NEVER idles.
//!
//! `repo_status` must be warm and fresh for `zdashboard` / `zstatus --all` to be
//! instant *and* accurate, and a live dirtiness scan over thousands of repos is
//! too slow to do on demand. So the daemon runs a dedicated pool — one worker per
//! core — that perpetually sweeps the index: each worker pulls the next repo off a
//! shared rotating cursor, computes its status, and hands the result to a single
//! writer thread, then immediately grabs the next. No pauses; the pool is always
//! working, so every repo's status is refreshed every few seconds.
//!
//! Compute (the expensive `is_dirty` worktree scan) is parallel and read-only;
//! **one** writer batches the results into the db. That split matters: SQLite's
//! WAL allows a single writer at a time, so having every worker write directly
//! would thrash on the write lock. The sweep is a single always-running "status
//! maintainer" ledger job whose output reports live throughput, so `zjobs`/`zjob`
//! show it working. `zvcs.statusinterval = 0` disables it.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

/// One repo to keep fresh: its ledger id and git dir.
struct Target {
    id: i64,
    git_dir: PathBuf,
}

/// A computed status result headed for the writer.
struct Update {
    id: i64,
    dirty: bool,
    detached: bool,
    sync: String,
    head: String,
}

/// Spawn the maintainer pool on the daemon, unless disabled.
pub fn spawn_if_enabled() {
    if interval_secs() == 0 {
        return; // `zvcs.statusinterval = 0` turns the maintainer off.
    }
    let workers = thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let cursor = Arc::new(AtomicUsize::new(0));
    let snapshot: Arc<RwLock<Arc<Vec<Target>>>> = Arc::new(RwLock::new(Arc::new(load_targets())));
    let (tx, rx) = mpsc::channel::<Update>();

    // Read-only compute workers: each perpetually claims the next repo off the
    // shared cursor, computes its status, and sends it to the writer. No db
    // writes here, so there is no write-lock contention between them.
    for _ in 0..workers {
        let cursor = Arc::clone(&cursor);
        let snapshot = Arc::clone(&snapshot);
        let tx = tx.clone();
        thread::spawn(move || loop {
            let repos = { snapshot.read().unwrap().clone() };
            if repos.is_empty() {
                thread::sleep(Duration::from_millis(500));
                continue;
            }
            let target = &repos[cursor.fetch_add(1, Ordering::Relaxed) % repos.len()];
            if let Ok(repo) = gix::open(&target.git_dir) {
                let (dirty, detached, sync, head) = crate::superset::status::compute(&repo);
                // A closed channel means the writer is gone → this pool is done.
                if tx.send(Update { id: target.id, dirty, detached, sync, head }).is_err() {
                    return;
                }
            }
        });
    }
    drop(tx); // only the workers hold senders now; the writer stops when all exit.

    // The single writer: batches results into the db, refreshes the repo snapshot
    // (new repos join, deleted drop), and publishes live throughput to the job.
    thread::spawn(move || writer_loop(rx, snapshot, workers));
}

/// Drain computed updates, batch-write them, and keep the snapshot + job current.
fn writer_loop(rx: mpsc::Receiver<Update>, snapshot: Arc<RwLock<Arc<Vec<Target>>>>, workers: usize) {
    let job = create_job();
    let mut conn = crate::db::open_rw().ok();
    let mut batch: Vec<Update> = Vec::new();
    let mut writes: u64 = 0;
    let mut last_tick = Instant::now();
    let mut last_writes: u64 = 0;

    loop {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(update) => {
                batch.push(update);
                if batch.len() >= 256 {
                    flush(&mut conn, &mut batch, &mut writes);
                }
            }
            Err(RecvTimeoutError::Timeout) => flush(&mut conn, &mut batch, &mut writes),
            Err(RecvTimeoutError::Disconnected) => {
                flush(&mut conn, &mut batch, &mut writes);
                break;
            }
        }
        if last_tick.elapsed() >= Duration::from_secs(5) {
            let fresh = load_targets();
            let indexed = fresh.len();
            *snapshot.write().unwrap() = Arc::new(fresh);
            let rate = writes.saturating_sub(last_writes) / 5;
            last_writes = writes;
            last_tick = Instant::now();
            publish(&conn, job, workers, indexed, writes, rate);
        }
    }
}

/// Write a batch of status rows in one transaction. Reopens the connection on a
/// hard error (rare, since this is the only writer).
fn flush(conn: &mut Option<rusqlite::Connection>, batch: &mut Vec<Update>, writes: &mut u64) {
    if batch.is_empty() {
        return;
    }
    if conn.is_none() {
        *conn = crate::db::open_rw().ok();
    }
    if let Some(c) = conn {
        let mut ok = true;
        let _ = c.execute_batch("BEGIN");
        for u in batch.iter() {
            if crate::db::upsert_status(c, u.id, u.dirty, u.detached, &u.sync, &u.head).is_err() {
                ok = false;
                break;
            }
        }
        if ok {
            let _ = c.execute_batch("COMMIT");
            *writes += batch.len() as u64;
        } else {
            let _ = c.execute_batch("ROLLBACK");
            *conn = None; // reopen next flush
        }
    }
    batch.clear();
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
fn publish(conn: &Option<rusqlite::Connection>, job: Option<i64>, workers: usize, indexed: usize, writes: u64, rate: u64) {
    let (Some(id), Some(c)) = (job, conn) else {
        return;
    };
    let out = format!("{workers} workers · {indexed} repos indexed · {writes} status writes · ~{rate}/s");
    let _ = c.execute(
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
