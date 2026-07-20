//! SQLite job ledger + repo index at `~/.zvcs/db.sqlite` (WAL).
//!
//! Two tables:
//!   * `repos` — every git repository the daemon has indexed (crawler + the
//!     working repo). The "index all git repos on the storage device" store.
//!   * `jobs` — the ledger: async `z`-verb jobs *and* autonomous-op failures.
//!     A failed autobump/reconcile lands here as a `failed` row, which the next
//!     `git` invocation surfaces (notify-on-next-command) — the only channel an
//!     async/headless failure has, since it carries no exit code back.
//!
//! There is exactly one daemon process, so writes go through a single write
//! connection (WAL) and never corrupt; client read verbs (`zjobs`/`zjob`/
//! `zrepos`) open the db read-only and run concurrently with the daemon, and
//! keep working even when the daemon is down.

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS repos (
    id            INTEGER PRIMARY KEY,
    git_dir       TEXT UNIQUE NOT NULL,
    workdir       TEXT,
    discovered_at INTEGER,
    last_seen     INTEGER
);
CREATE TABLE IF NOT EXISTS jobs (
    id            INTEGER PRIMARY KEY,
    repo_id       INTEGER REFERENCES repos(id),
    kind          TEXT NOT NULL,
    spec          TEXT,
    session_key   TEXT,
    state         TEXT NOT NULL,
    exit_code     INTEGER,
    sha_before    TEXT,
    sha_after     TEXT,
    output        TEXT,
    parent_job_id INTEGER,
    notified_at   INTEGER,
    created_at    INTEGER,
    started_at    INTEGER,
    finished_at   INTEGER
);
CREATE INDEX IF NOT EXISTS jobs_repo_state ON jobs(repo_id, state);
";

/// `~/.zvcs/db.sqlite` (honors `ZVCS_HOME`).
pub fn db_path() -> PathBuf {
    crate::superset::zdaemon::zvcs_home().join("db.sqlite")
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Open the read-write connection (daemon side): WAL, busy-timeout, schema.
pub fn open_rw() -> Result<Connection> {
    let conn = Connection::open(db_path()).context("open db (rw)")?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    conn.execute_batch(SCHEMA)?;
    Ok(conn)
}

/// Open a read-only connection (client side). Errors if the db does not exist
/// yet — callers treat that as "no ledger, nothing to show".
pub fn open_ro() -> Result<Connection> {
    let conn = Connection::open_with_flags(db_path(), OpenFlags::SQLITE_OPEN_READ_ONLY)
        .context("open db (ro)")?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    Ok(conn)
}

/// Insert or refresh a repo row, returning its id.
pub fn upsert_repo(conn: &Connection, git_dir: &Path, workdir: Option<&Path>) -> Result<i64> {
    let gd = git_dir.to_string_lossy();
    let wd = workdir.map(|p| p.to_string_lossy().into_owned());
    let ts = now();
    conn.execute(
        "INSERT INTO repos (git_dir, workdir, discovered_at, last_seen)
         VALUES (?1, ?2, ?3, ?3)
         ON CONFLICT(git_dir) DO UPDATE SET last_seen = ?3, workdir = ?2",
        rusqlite::params![gd, wd, ts],
    )?;
    let id: i64 = conn.query_row(
        "SELECT id FROM repos WHERE git_dir = ?1",
        [gd],
        |r| r.get(0),
    )?;
    Ok(id)
}

/// One row of the repo index.
pub struct RepoRow {
    pub id: i64,
    pub git_dir: String,
    pub workdir: Option<String>,
}

pub fn list_repos(conn: &Connection) -> Result<Vec<RepoRow>> {
    let mut stmt = conn.prepare("SELECT id, git_dir, workdir FROM repos ORDER BY git_dir")?;
    let rows = stmt
        .query_map([], |r| {
            Ok(RepoRow {
                id: r.get(0)?,
                git_dir: r.get(1)?,
                workdir: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Insert a queued job, returning its id (the number shown to the user).
pub fn insert_job(
    conn: &Connection,
    repo_id: i64,
    kind: &str,
    spec: &str,
    session: Option<&str>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO jobs (repo_id, kind, spec, session_key, state, created_at)
         VALUES (?1, ?2, ?3, ?4, 'queued', ?5)",
        rusqlite::params![repo_id, kind, spec, session, now()],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn job_running(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE jobs SET state='running', started_at=?2 WHERE id=?1",
        rusqlite::params![id, now()],
    )?;
    Ok(())
}

/// Finalize a job: `done` or `failed`, with output/exit captured.
pub fn job_finished(
    conn: &Connection,
    id: i64,
    state: &str,
    exit_code: i32,
    output: &str,
    sha_after: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE jobs SET state=?2, exit_code=?3, output=?4, sha_after=?5, finished_at=?6 WHERE id=?1",
        rusqlite::params![id, state, exit_code, output, sha_after, now()],
    )?;
    Ok(())
}

/// Record an autonomous-op failure (autobump/reconcile) so the next `git`
/// invocation can surface it. Upserts the repo, then a `failed` job row.
pub fn record_failure(git_dir: &Path, kind: &str, reason: &str) -> Result<()> {
    // Canonicalize so the write here and the read in notify-on-next-command key
    // on the same path string.
    let git_dir = git_dir.canonicalize().unwrap_or_else(|_| git_dir.to_path_buf());
    let conn = open_rw()?;
    let repo_id = upsert_repo(&conn, &git_dir, None)?;
    let ts = now();
    conn.execute(
        "INSERT INTO jobs (repo_id, kind, spec, state, output, created_at, finished_at)
         VALUES (?1, ?2, NULL, 'failed', ?3, ?4, ?4)",
        rusqlite::params![repo_id, kind, reason, ts],
    )?;
    Ok(())
}

/// One ledger row, joined with its repo path.
pub struct JobRow {
    pub id: i64,
    pub kind: String,
    pub state: String,
    pub git_dir: Option<String>,
    pub output: Option<String>,
    pub exit_code: Option<i64>,
    pub sha_after: Option<String>,
}

pub fn list_jobs(conn: &Connection, limit: i64) -> Result<Vec<JobRow>> {
    let mut stmt = conn.prepare(
        "SELECT j.id, j.kind, j.state, r.git_dir, j.output, j.exit_code, j.sha_after
         FROM jobs j LEFT JOIN repos r ON r.id = j.repo_id
         ORDER BY j.id DESC LIMIT ?1",
    )?;
    let rows = stmt
        .query_map([limit], job_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn get_job(conn: &Connection, id: i64) -> Result<Option<JobRow>> {
    let mut stmt = conn.prepare(
        "SELECT j.id, j.kind, j.state, r.git_dir, j.output, j.exit_code, j.sha_after
         FROM jobs j LEFT JOIN repos r ON r.id = j.repo_id
         WHERE j.id = ?1",
    )?;
    let row = stmt.query_row([id], job_row).optional()?;
    Ok(row)
}

fn job_row(r: &rusqlite::Row) -> rusqlite::Result<JobRow> {
    Ok(JobRow {
        id: r.get(0)?,
        kind: r.get(1)?,
        state: r.get(2)?,
        git_dir: r.get(3)?,
        output: r.get(4)?,
        exit_code: r.get(5)?,
        sha_after: r.get(6)?,
    })
}

/// Pending (unnotified) failed jobs for the repo at `git_dir` — the
/// notify-on-next-command source. Returns `(id, kind, reason)`.
pub fn pending_failures(conn: &Connection, git_dir: &Path) -> Result<Vec<(i64, String, String)>> {
    let gd = git_dir.to_string_lossy();
    let mut stmt = conn.prepare(
        "SELECT j.id, j.kind, COALESCE(j.output, '')
         FROM jobs j JOIN repos r ON r.id = j.repo_id
         WHERE r.git_dir = ?1 AND j.state = 'failed' AND j.notified_at IS NULL
         ORDER BY j.id",
    )?;
    let rows = stmt
        .query_map([gd], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn mark_notified(conn: &Connection, ids: &[i64]) -> Result<()> {
    let ts = now();
    for id in ids {
        conn.execute(
            "UPDATE jobs SET notified_at=?2 WHERE id=?1",
            rusqlite::params![id, ts],
        )?;
    }
    Ok(())
}
