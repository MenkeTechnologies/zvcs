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
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

/// Once the cache is warm (first full sweep done), each worker sleeps this long
/// between repos so the pool stops pegging the CPU. Hungry first, gentle after.
const WARM_THROTTLE: Duration = Duration::from_millis(400);

/// One repo to keep fresh: its ledger id, git dir, working-tree root, and the
/// root mtime recorded at its last scan (`None` = never scanned).
struct Target {
    id: i64,
    git_dir: PathBuf,
    workdir: PathBuf,
    mtime: Option<i64>,
}

/// A computed status result headed for the writer.
struct Update {
    id: i64,
    dirty: bool,
    detached: bool,
    sync: String,
    head: String,
    /// The root mtime observed for this scan, persisted so the next sweep can skip
    /// this repo while its working tree is untouched.
    mtime: Option<i64>,
}

/// Spawn the maintainer pool on the daemon, unless disabled.
pub fn spawn_if_enabled() {
    if interval_secs() == 0 {
        return; // `zvcs.statusinterval = 0` turns the maintainer off.
    }
    let workers = thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let initial = load_targets();
    let initial_n = initial.len().max(1);
    let cursor = Arc::new(AtomicUsize::new(0));
    let snapshot: Arc<RwLock<Arc<Vec<Target>>>> = Arc::new(RwLock::new(Arc::new(initial)));
    // `warm` flips true after the first full sweep; workers then throttle so the
    // daemon is hungry on the cold cache and gentle once it is maintained.
    let warm = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel::<Update>();

    // Read-only compute workers: each perpetually claims the next repo off the
    // shared cursor, computes its status, and sends it to the writer. No db
    // writes here, so there is no write-lock contention between them.
    for _ in 0..workers {
        let cursor = Arc::clone(&cursor);
        let snapshot = Arc::clone(&snapshot);
        let warm = Arc::clone(&warm);
        let tx = tx.clone();
        thread::spawn(move || loop {
            let repos = { snapshot.read().unwrap().clone() };
            if repos.is_empty() {
                thread::sleep(Duration::from_millis(500));
                continue;
            }
            let target = &repos[cursor.fetch_add(1, Ordering::Relaxed) % repos.len()];
            // Cheap mtime gate: if the working-tree root hasn't been touched since
            // the last scan, skip the expensive `is_dirty` compute entirely — the
            // cached status is still valid. This is what keeps the pool from
            // pegging cores re-scanning thousands of untouched repos.
            let cur = crate::db::root_mtime(&target.workdir);
            if let (Some(c), Some(m)) = (cur, target.mtime) {
                if c <= m {
                    if warm.load(Ordering::Relaxed) {
                        thread::sleep(WARM_THROTTLE);
                    }
                    continue;
                }
            }
            if let Ok(repo) = gix::open(&target.git_dir) {
                let (dirty, detached, sync, head) = crate::superset::status::compute(&repo);
                // A closed channel means the writer is gone → this pool is done.
                if tx.send(Update { id: target.id, dirty, detached, sync, head, mtime: cur }).is_err() {
                    return;
                }
            }
            // Full speed until warm; then ease off so the pool stops pegging cores.
            if warm.load(Ordering::Relaxed) {
                thread::sleep(WARM_THROTTLE);
            }
        });
    }
    drop(tx); // only the workers hold senders now; the writer stops when all exit.

    // The single writer: batches results into the db, refreshes the repo snapshot
    // (new repos join, deleted drop), flips `warm` after the first sweep, and
    // publishes live throughput to the job.
    thread::spawn(move || writer_loop(rx, snapshot, workers, warm, initial_n));
}

/// Drain computed updates, batch-write them, and keep the snapshot + job current.
fn writer_loop(
    rx: mpsc::Receiver<Update>,
    snapshot: Arc<RwLock<Arc<Vec<Target>>>>,
    workers: usize,
    warm: Arc<AtomicBool>,
    initial_n: usize,
) {
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
        // Warm once the first sweep's worth of writes has landed → workers throttle.
        if !warm.load(Ordering::Relaxed) && writes >= initial_n as u64 {
            warm.store(true, Ordering::Relaxed);
        }
        if last_tick.elapsed() >= Duration::from_secs(5) {
            let fresh = load_targets();
            let indexed = fresh.len();
            *snapshot.write().unwrap() = Arc::new(fresh);
            let rate = writes.saturating_sub(last_writes) / 5;
            last_writes = writes;
            last_tick = Instant::now();
            let phase = if warm.load(Ordering::Relaxed) { "maintaining" } else { "warming" };
            publish(&conn, job, workers, indexed, writes, rate, phase);
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
            // Record the root mtime we scanned at, so the next sweep skips this repo
            // until its working tree is touched again.
            if let Some(mt) = u.mtime {
                crate::db::set_repo_mtime_by_id(c, u.id, mt);
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

/// Load every indexed repo as a [`Target`], including its last-scan root mtime so
/// the workers can skip untouched repos.
fn load_targets() -> Vec<Target> {
    let Ok(conn) = crate::db::open_ro() else { return Vec::new() };
    let Ok(mut stmt) = conn.prepare("SELECT id, git_dir, workdir, mtime FROM repos") else {
        return Vec::new();
    };
    let rows = stmt.query_map([], |r| {
        let git_dir: String = r.get(1)?;
        let workdir: Option<String> = r.get(2)?;
        Ok(Target {
            id: r.get(0)?,
            workdir: workdir.map(PathBuf::from).unwrap_or_else(|| PathBuf::from(&git_dir)),
            git_dir: PathBuf::from(git_dir),
            mtime: r.get(3)?,
        })
    });
    match rows {
        Ok(it) => it.filter_map(Result::ok).collect(),
        Err(_) => Vec::new(),
    }
}

/// Insert the always-running "status maintainer" ledger row, returning its id.
fn create_job() -> Option<i64> {
    let conn = crate::db::open_rw().ok()?;
    let id = crate::db::insert_job(&conn, None, "status maintainer", "{\"kind\":\"statusd\"}", None).ok()?;
    let _ = crate::db::job_running(&conn, id);
    Some(id)
}

/// Update the maintainer job's live progress line (kept in the `running` state).
fn publish(conn: &Option<rusqlite::Connection>, job: Option<i64>, workers: usize, indexed: usize, writes: u64, rate: u64, phase: &str) {
    let (Some(id), Some(c)) = (job, conn) else {
        return;
    };
    let out = format!("{phase} · {workers} workers · {indexed} repos indexed · {writes} status writes · ~{rate}/s");
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
