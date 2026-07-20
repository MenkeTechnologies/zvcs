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
CREATE TABLE IF NOT EXISTS claims (
    repo_id    INTEGER PRIMARY KEY REFERENCES repos(id),
    session    TEXT NOT NULL,
    workdir    TEXT,
    claimed_at INTEGER
);
CREATE TABLE IF NOT EXISTS repo_status (
    repo_id    INTEGER PRIMARY KEY REFERENCES repos(id),
    dirty      INTEGER,
    detached   INTEGER,
    sync       TEXT,
    head       TEXT,
    updated_at INTEGER
);
CREATE TABLE IF NOT EXISTS snapshots (
    name       TEXT NOT NULL,
    git_dir    TEXT NOT NULL,
    workdir    TEXT,
    sha        TEXT NOT NULL,
    created_at INTEGER
);
CREATE INDEX IF NOT EXISTS snapshots_name ON snapshots(name);
CREATE TABLE IF NOT EXISTS worktrees (
    name       TEXT PRIMARY KEY,
    path       TEXT NOT NULL,
    created_at INTEGER
);
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

/// Remove repos whose git-dir no longer exists on disk (deleted since indexing).
/// Returns the number pruned. Old jobs keep their `repo_id` (the join tolerates a
/// missing repo), so history is preserved.
pub fn prune_missing(conn: &Connection) -> Result<usize> {
    let mut stmt = conn.prepare("SELECT id, git_dir FROM repos")?;
    let rows: Vec<(i64, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    let mut removed = 0;
    for (id, git_dir) in rows {
        if !std::path::Path::new(&git_dir).exists() {
            conn.execute("DELETE FROM repos WHERE id=?1", [id])?;
            removed += 1;
        }
    }
    Ok(removed)
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

/// Current state of a job, if it exists.
pub fn job_state(conn: &Connection, id: i64) -> Result<Option<String>> {
    let state = conn
        .query_row("SELECT state FROM jobs WHERE id=?1", [id], |r| r.get(0))
        .optional()?;
    Ok(state)
}

/// Flip a still-`queued` job to `stopped` (a stop that arrives before the worker
/// picks it up). Returns true if a queued job was actually stopped.
pub fn stop_if_queued(conn: &Connection, id: i64) -> Result<bool> {
    let n = conn.execute(
        "UPDATE jobs SET state='stopped', finished_at=?2 WHERE id=?1 AND state='queued'",
        rusqlite::params![id, now()],
    )?;
    Ok(n > 0)
}

/// Clone a job into a new `queued` row linked by `parent_job_id`, for restart.
/// Returns `(new_id, spec_json)` to enqueue, or `None` if the job is unknown.
pub fn restart_job(conn: &Connection, id: i64) -> Result<Option<(i64, String)>> {
    let row: Option<(Option<i64>, String, Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT repo_id, kind, spec, session_key FROM jobs WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((repo_id, kind, spec, session)) = row else {
        return Ok(None);
    };
    let spec = spec.unwrap_or_default();
    conn.execute(
        "INSERT INTO jobs (repo_id, kind, spec, session_key, state, parent_job_id, created_at)
         VALUES (?1, ?2, ?3, ?4, 'queued', ?5, ?6)",
        rusqlite::params![repo_id, kind, spec, session, id, now()],
    )?;
    Ok(Some((conn.last_insert_rowid(), spec)))
}

/// Insert or refresh a repo's cached status.
pub fn upsert_status(
    conn: &Connection,
    repo_id: i64,
    dirty: bool,
    detached: bool,
    sync: &str,
    head: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO repo_status (repo_id, dirty, detached, sync, head, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(repo_id) DO UPDATE SET
             dirty=?2, detached=?3, sync=?4, head=?5, updated_at=?6",
        rusqlite::params![repo_id, dirty as i64, detached as i64, sync, head, now()],
    )?;
    Ok(())
}

/// One cached status row: `(path, dirty, detached, sync, head)`.
pub struct StatusRow {
    pub path: String,
    pub dirty: bool,
    pub detached: bool,
    pub sync: String,
    pub head: String,
}

/// All cached repo statuses, joined with the repo path.
pub fn list_status(conn: &Connection) -> Result<Vec<StatusRow>> {
    let mut stmt = conn.prepare(
        "SELECT COALESCE(r.workdir, r.git_dir), s.dirty, s.detached, s.sync, s.head
         FROM repo_status s JOIN repos r ON r.id = s.repo_id
         ORDER BY r.workdir",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(StatusRow {
                path: r.get(0)?,
                dirty: r.get::<_, i64>(1)? != 0,
                detached: r.get::<_, i64>(2)? != 0,
                sync: r.get(3)?,
                head: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Save a tree-wide snapshot: replace any existing rows for `name` with the given
/// `(git_dir, workdir, sha)` entries.
pub fn save_snapshot(conn: &Connection, name: &str, entries: &[(String, String, String)]) -> Result<()> {
    conn.execute("DELETE FROM snapshots WHERE name = ?1", [name])?;
    let ts = now();
    for (git_dir, workdir, sha) in entries {
        conn.execute(
            "INSERT INTO snapshots (name, git_dir, workdir, sha, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![name, git_dir, workdir, sha, ts],
        )?;
    }
    Ok(())
}

/// Load a snapshot's entries as `(git_dir, workdir, sha)`.
pub fn load_snapshot(conn: &Connection, name: &str) -> Result<Vec<(String, String, String)>> {
    let mut stmt = conn.prepare("SELECT git_dir, workdir, sha FROM snapshots WHERE name = ?1 ORDER BY git_dir")?;
    let rows = stmt
        .query_map([name], |r| Ok((r.get(0)?, r.get::<_, Option<String>>(1)?.unwrap_or_default(), r.get(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// List snapshot names with their repo count.
pub fn list_snapshots(conn: &Connection) -> Result<Vec<(String, i64)>> {
    let mut stmt = conn.prepare("SELECT name, COUNT(*) FROM snapshots GROUP BY name ORDER BY name")?;
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Record a zvcs-managed worktree.
pub fn add_worktree(conn: &Connection, name: &str, path: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO worktrees (name, path, created_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(name) DO UPDATE SET path=?2, created_at=?3",
        rusqlite::params![name, path, now()],
    )?;
    Ok(())
}

/// List zvcs-managed worktrees as `(name, path)`.
pub fn list_worktrees(conn: &Connection) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare("SELECT name, path FROM worktrees ORDER BY name")?;
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// The recorded path of a worktree, if any.
pub fn worktree_path(conn: &Connection, name: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT path FROM worktrees WHERE name=?1", [name], |r| r.get(0))
        .optional()?)
}

pub fn remove_worktree(conn: &Connection, name: &str) -> Result<()> {
    conn.execute("DELETE FROM worktrees WHERE name=?1", [name])?;
    Ok(())
}

/// Outcome of a claim attempt.
pub enum ClaimResult {
    /// The claim was newly acquired by this session.
    Acquired,
    /// This session already held the claim.
    AlreadyMine,
    /// Another session holds it (carries that session).
    HeldBy(String),
}

/// Claim `repo_id` for `session` (one claim per repo, race-safe via the PK).
pub fn claim(conn: &Connection, repo_id: i64, session: &str, workdir: Option<&str>) -> Result<ClaimResult> {
    let held: Option<String> = conn
        .query_row("SELECT session FROM claims WHERE repo_id=?1", [repo_id], |r| r.get(0))
        .optional()?;
    if let Some(s) = held {
        return Ok(if s == session { ClaimResult::AlreadyMine } else { ClaimResult::HeldBy(s) });
    }
    match conn.execute(
        "INSERT INTO claims (repo_id, session, workdir, claimed_at) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![repo_id, session, workdir, now()],
    ) {
        Ok(_) => Ok(ClaimResult::Acquired),
        Err(_) => {
            // Lost a race — report the winner.
            let s: String = conn.query_row("SELECT session FROM claims WHERE repo_id=?1", [repo_id], |r| r.get(0))?;
            Ok(if s == session { ClaimResult::AlreadyMine } else { ClaimResult::HeldBy(s) })
        }
    }
}

/// Release `repo_id`'s claim if held by `session`. Returns true if a claim was removed.
pub fn unclaim(conn: &Connection, repo_id: i64, session: &str) -> Result<bool> {
    let n = conn.execute(
        "DELETE FROM claims WHERE repo_id=?1 AND session=?2",
        rusqlite::params![repo_id, session],
    )?;
    Ok(n > 0)
}

/// All active claims as `(path, session, claimed_at)` — `path` is the workdir,
/// falling back to the git dir.
pub fn list_claims(conn: &Connection) -> Result<Vec<(String, String, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT COALESCE(c.workdir, r.git_dir), c.session, c.claimed_at
         FROM claims c JOIN repos r ON r.id = c.repo_id
         ORDER BY c.claimed_at",
    )?;
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
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
