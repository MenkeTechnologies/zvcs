//! Bounded async-job pool + cancellation registry, owned by the daemon.
//!
//! `init(n)` starts `n` worker threads draining a shared queue, so at most `n`
//! jobs execute concurrently (the backlog waits in the channel). Each running
//! job registers a [`Cancel`] handle so `zjob stop` can abort it — killing the
//! current child process if it is already running, or flipping a still-queued job
//! to `stopped` before a worker picks it up. `restart` is a ledger clone linked
//! by `parent_job_id`, re-enqueued here.

use crate::jobrun::{self, Cancel};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;

/// `(job_id, spec_json)`.
type Job = (i64, String);

static POOL: OnceLock<Sender<Job>> = OnceLock::new();
static REG: OnceLock<Mutex<HashMap<i64, Cancel>>> = OnceLock::new();

fn registry() -> &'static Mutex<HashMap<i64, Cancel>> {
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Start the pool with `workers` threads (min 1). First call wins.
pub fn init(workers: usize) {
    let _ = registry();
    POOL.get_or_init(|| {
        let (tx, rx): (Sender<Job>, Receiver<Job>) = channel();
        let rx = Arc::new(Mutex::new(rx));
        for _ in 0..workers.max(1) {
            let rx = Arc::clone(&rx);
            thread::spawn(move || loop {
                let job = rx.lock().unwrap().recv();
                match job {
                    Ok((id, spec)) => run_job(id, spec),
                    Err(_) => break, // sender dropped
                }
            });
        }
        tx
    });
}

/// Enqueue a job. Falls back to a dedicated thread if the pool wasn't started.
pub fn submit(id: i64, spec: String) {
    if let Some(tx) = POOL.get() {
        // On a send failure the value comes back in the error, so we can still
        // fall back without cloning.
        match tx.send((id, spec)) {
            Ok(()) => return,
            Err(e) => {
                let (id, spec) = e.0;
                thread::spawn(move || run_job(id, spec));
            }
        }
    } else {
        thread::spawn(move || run_job(id, spec));
    }
}

/// Stop a job: cancel + kill if running, else mark a queued job `stopped`.
/// Returns true if the job was known and actionable.
pub fn stop(id: i64) -> bool {
    if let Some(cancel) = registry().lock().unwrap().get(&id).cloned() {
        cancel.cancel();
        return true;
    }
    if let Ok(conn) = crate::db::open_rw() {
        return crate::db::stop_if_queued(&conn, id).unwrap_or(false);
    }
    false
}

fn run_job(id: i64, spec_json: String) {
    // Register the cancel handle FIRST, so a `zjob stop` arriving during startup
    // finds it (and its flag is honored by `execute`), then atomically claim the
    // job `queued` → `running`. If the claim fails, a stop already flipped it to
    // `stopped` while it was queued — bail without running it.
    let cancel = Cancel::default();
    registry().lock().unwrap().insert(id, cancel.clone());
    match crate::db::open_rw() {
        Ok(conn) => {
            if !crate::db::claim_running(&conn, id).unwrap_or(false) {
                registry().lock().unwrap().remove(&id);
                return; // was stopped-while-queued (or vanished)
            }
        }
        Err(_) => {
            registry().lock().unwrap().remove(&id);
            return;
        }
    }

    let result = match serde_json::from_str::<Value>(&spec_json) {
        Ok(spec) => jobrun::execute(&spec, &cancel),
        Err(e) => jobrun::JobResult {
            ok: false,
            output: format!("invalid job spec: {e}\n"),
            sha_after: None,
            cancelled: false,
        },
    };

    registry().lock().unwrap().remove(&id);

    // `ok` wins over `cancelled`: a job whose commit/push actually succeeded is
    // `done`, even if a `zjob stop` raced in after the work completed. (A stop
    // that lands *during* a step already forces `ok=false` via `run`, so a truly
    // cancelled job is `!ok` → `stopped`.) Reporting a landed commit/push as
    // `stopped` would make the user re-submit it → duplicate commit/push.
    let state = if result.ok {
        "done"
    } else if result.cancelled {
        "stopped"
    } else {
        "failed"
    };
    let exit = if result.ok { 0 } else { 1 };

    // Retry the finalize so transient lock contention (SQLITE_BUSY — expected with
    // many concurrent instances) can't strand the row in `running` forever with
    // its output/sha lost. open_rw already carries a 5s busy-timeout; this adds a
    // few more attempts on top for a hard-contended finalize.
    for attempt in 0..5u32 {
        let wrote = crate::db::open_rw().and_then(|conn| {
            crate::db::job_finished(&conn, id, state, exit, &result.output, result.sha_after.as_deref())
        });
        if wrote.is_ok() || attempt == 4 {
            break;
        }
        thread::sleep(std::time::Duration::from_millis(100));
    }
}
