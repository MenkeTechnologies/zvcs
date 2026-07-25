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
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS repos (
    id            INTEGER PRIMARY KEY,
    git_dir       TEXT UNIQUE NOT NULL,
    workdir       TEXT,
    discovered_at INTEGER,
    last_seen     INTEGER,
    hook          TEXT,
    mtime         INTEGER,
    pinned        INTEGER
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
    head_sha   TEXT,
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
CREATE TABLE IF NOT EXISTS stashes (
    name       TEXT NOT NULL,
    git_dir    TEXT NOT NULL,
    workdir    TEXT NOT NULL,
    created_at INTEGER
);
CREATE INDEX IF NOT EXISTS stashes_name ON stashes(name);
CREATE TABLE IF NOT EXISTS triggers (
    path        TEXT PRIMARY KEY,
    command     TEXT NOT NULL,
    created_at  INTEGER,
    throttle_ms INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS events (
    id         INTEGER PRIMARY KEY,
    ts         INTEGER NOT NULL,
    kind       TEXT NOT NULL,
    repo_id    INTEGER REFERENCES repos(id),
    detail     TEXT,
    sha_before TEXT,
    sha_after  TEXT
);
-- The unified live feed behind `git zevents`/`ztail`. Triggers turn every
-- repo_status change into a typed event, so BOTH the whole-tree poller (statusd)
-- and the instant notify path (watch) feed it through `upsert_status` with no
-- caller changes. They fire on UPDATE only — a repo's first (INSERT) status row
-- never floods the feed at cold start. Reconciles are recorded explicitly by the
-- daemon's reconcile path (they do not reliably move through upsert_status).
-- `ev_commit` is installed from EV_COMMIT_SQL in open_rw (it was revised to key on
-- the commit SHA, not the invariant branch name, so DROP+CREATE beats IF NOT EXISTS).
CREATE TRIGGER IF NOT EXISTS ev_status AFTER UPDATE ON repo_status
    WHEN NEW.dirty IS NOT OLD.dirty OR NEW.sync IS NOT OLD.sync
    BEGIN
        INSERT INTO events (ts, kind, repo_id, detail, sha_before, sha_after)
        VALUES (CAST(strftime('%s','now') AS INTEGER), 'status', NEW.repo_id,
                CASE WHEN NEW.dirty IS NOT OLD.dirty
                     THEN (CASE WHEN NEW.dirty THEN 'dirty' ELSE 'clean' END)
                     ELSE NEW.sync END,
                NULL, NULL);
    END;
-- Inter-agent messages (`zbroadcast`/`zhandoff`). `to_session` NULL = broadcast.
-- Read state is per (message, session) so a broadcast can be marked read once per
-- agent without a mutable flag on the shared row.
CREATE TABLE IF NOT EXISTS messages (
    id           INTEGER PRIMARY KEY,
    ts           INTEGER NOT NULL,
    from_session TEXT,
    to_session   TEXT,
    body         TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS message_reads (
    message_id INTEGER NOT NULL,
    session    TEXT NOT NULL,
    PRIMARY KEY (message_id, session)
);
-- Semantic-event subscriptions (`zon`): the daemon runs `command` when a feed
-- event matches `kind` (NULL = any) and `repo_like` (NULL = any repo).
CREATE TABLE IF NOT EXISTS subscriptions (
    id         INTEGER PRIMARY KEY,
    kind       TEXT,
    repo_like  TEXT,
    command    TEXT NOT NULL,
    created_at INTEGER
);
-- Per-process commit ledger (`zppid`, `zdashboard`). One row per durable process
-- responsible for commits — the agent/program/interactive shell found by walking up
-- past transient per-command wrapper shells (see superset::zppid). Keyed by an
-- explicit `ZVCS_SESSION` when set, else `pid-<responsible-pid>`. `ppid` is that
-- durable pid, `cmd`/`cwd` its command name and working directory (so the pid is
-- legible), all refreshed on each commit. `commits` accumulates every
-- commit-producing verb that advanced HEAD. A pure client-side counter — the daemon
-- never touches it.
CREATE TABLE IF NOT EXISTS ppids (
    session    TEXT PRIMARY KEY,
    ppid       INTEGER,
    cmd        TEXT,
    cwd        TEXT,
    commits    INTEGER NOT NULL DEFAULT 0,
    first_seen INTEGER,
    last_seen  INTEGER
);
-- Per-process command tally (`zprocs`): how many of each mutating verb (commit,
-- push, add, merge, …) a durable process has run. Keyed by the same session as
-- `ppids` (join on it for the process's pid/cmd/cwd), one row per (session, verb).
-- Only mutating verbs are recorded, so the read hot path never writes here.
CREATE TABLE IF NOT EXISTS treediff (
    old_tree TEXT NOT NULL,
    new_tree TEXT NOT NULL,
    counts   INTEGER NOT NULL,
    files    TEXT NOT NULL,
    PRIMARY KEY (old_tree, new_tree, counts)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS blame (
    -- `commit` is a SQLite keyword: naming the column that made the whole SCHEMA
    -- batch fail to parse, which silently disabled every ledger write, not just
    -- this table.
    commit_oid TEXT NOT NULL,
    path   TEXT NOT NULL,
    algo   TEXT NOT NULL,
    blob   TEXT NOT NULL,
    runs   TEXT NOT NULL,
    PRIMARY KEY (commit_oid, path, algo)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS abbrev (
    oid     TEXT NOT NULL,
    hex_len INTEGER NOT NULL,
    short   TEXT NOT NULL,
    PRIMARY KEY (oid, hex_len)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS proc_verbs (
    session    TEXT NOT NULL,
    verb       TEXT NOT NULL,
    count      INTEGER NOT NULL DEFAULT 0,
    first_seen INTEGER,
    last_seen  INTEGER,
    PRIMARY KEY (session, verb)
);
";

/// The commit-detection trigger, installed via DROP+CREATE in [`open_rw`] so a
/// definition change reaches existing dbs (`CREATE TRIGGER IF NOT EXISTS` would
/// keep the stale one). It keys on the peeled commit SHA — the branch name in
/// `head` is invariant across commits, so the original head-keyed trigger never
/// fired on a commit. Fires only when BOTH old and new `head_sha` are real commit
/// ids, so the NULL→sha backfill after an upgrade and unborn HEADs emit nothing.
const EV_COMMIT_SQL: &str = "
CREATE TRIGGER ev_commit AFTER UPDATE ON repo_status
    WHEN NEW.head_sha IS NOT OLD.head_sha
     AND OLD.head_sha IS NOT NULL AND OLD.head_sha <> ''
     AND NEW.head_sha IS NOT NULL AND NEW.head_sha <> ''
    BEGIN
        INSERT INTO events (ts, kind, repo_id, detail, sha_before, sha_after)
        VALUES (CAST(strftime('%s','now') AS INTEGER), 'commit', NEW.repo_id, NULL, OLD.head_sha, NEW.head_sha);
    END;
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

/// The cached file-change list between two trees.
///
/// A tree-to-tree diff is a pure function of two immutable trees, so like the
/// blame cache this never expires and is shared by every clone holding them.
/// `counts` is part of the key because the added/deleted line tallies require
/// reading each blob — a caller that does not need them stores a cheaper row.
pub fn treediff_load(
    conn: &Connection,
    old_tree: &str,
    new_tree: &str,
    counts: bool,
) -> Option<String> {
    conn.query_row(
        "SELECT files FROM treediff WHERE old_tree = ?1 AND new_tree = ?2 AND counts = ?3",
        rusqlite::params![old_tree, new_tree, i64::from(counts)],
        |r| r.get(0),
    )
    .optional()
    .ok()
    .flatten()
}

pub fn treediff_store(
    conn: &Connection,
    old_tree: &str,
    new_tree: &str,
    counts: bool,
    files: &str,
) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO treediff (old_tree, new_tree, counts, files) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![old_tree, new_tree, i64::from(counts), files],
    )?;
    Ok(())
}
/// The cached blame of one path at one commit: the blob that was blamed and a
/// run-length encoding of the attribution.
///
/// Blaming `(commit, path)` is deterministic over immutable objects, so the
/// answer never expires — and because commits are content addresses, an entry is
/// valid in every clone that holds them. `algo` is part of the key because the
/// diff algorithm changes the attribution.
pub fn blame_load(
    conn: &Connection,
    commit: &str,
    path: &str,
    algo: &str,
) -> Option<(String, String)> {
    conn.query_row(
        "SELECT blob, runs FROM blame WHERE commit_oid = ?1 AND path = ?2 AND algo = ?3",
        rusqlite::params![commit, path, algo],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
    .ok()
    .flatten()
}

pub fn blame_store(
    conn: &Connection,
    commit: &str,
    path: &str,
    algo: &str,
    blob: &str,
    runs: &str,
) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO blame (commit_oid, path, algo, blob, runs) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![commit, path, algo, blob, runs],
    )?;
    Ok(())
}
/// Load every cached abbreviation computed at `hex_len`.
///
/// Object ids are content addresses, so an abbreviation is valid for any repo
/// that holds the object — the cache is machine-wide and shared across clones.
/// `hex_len` is part of the key because the correct abbreviation grows with the
/// repository: an entry computed when 7 hex digits were unique must not be
/// served once the repo needs 8.
pub fn abbrev_load(conn: &Connection, hex_len: usize) -> Result<HashMap<String, String>> {
    let mut stmt = conn.prepare("SELECT oid, short FROM abbrev WHERE hex_len = ?1")?;
    let rows = stmt.query_map([hex_len as i64], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// Record abbreviations computed this run, in one transaction.
pub fn abbrev_store(conn: &mut Connection, hex_len: usize, pairs: &[(String, String)]) -> Result<()> {
    let tx = conn.transaction()?;
    {
        let mut stmt =
            tx.prepare("INSERT OR IGNORE INTO abbrev (oid, hex_len, short) VALUES (?1, ?2, ?3)")?;
        for (oid, short) in pairs {
            stmt.execute(rusqlite::params![oid, hex_len as i64, short])?;
        }
    }
    tx.commit()?;
    Ok(())
}
/// Open the read-write connection (daemon side): WAL, busy-timeout, schema.
pub fn open_rw() -> Result<Connection> {
    let conn = Connection::open(db_path()).context("open db (rw)")?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    conn.execute_batch(SCHEMA)?;
    migrate(&conn);
    Ok(conn)
}

/// Bump when a migration is added below.
const SCHEMA_VERSION: i64 = 3;

/// One-time migrations, gated by `PRAGMA user_version` so they run once per db
/// instead of on every connection. `open_rw` is on the hot path (every client
/// command that records anything), so the `ev_commit` DROP+CREATE especially must
/// not churn — it would rewrite the trigger, and briefly drop it, on each open.
fn migrate(conn: &Connection) {
    let ver: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap_or(0);
    if ver >= SCHEMA_VERSION {
        return;
    }
    // Columns added after the original schema. `CREATE TABLE IF NOT EXISTS` won't
    // add a column to an existing table, so ALTER explicitly; the "duplicate
    // column" error when it is already present is ignored.
    let _ = conn.execute("ALTER TABLE repos ADD COLUMN hook TEXT", []);
    let _ = conn.execute("ALTER TABLE triggers ADD COLUMN throttle_ms INTEGER NOT NULL DEFAULT 0", []);
    let _ = conn.execute("ALTER TABLE repos ADD COLUMN mtime INTEGER", []);
    let _ = conn.execute("ALTER TABLE repo_status ADD COLUMN head_sha TEXT", []);
    let _ = conn.execute("ALTER TABLE repos ADD COLUMN pinned INTEGER", []); // v2: zpin
    // v3: zppid gained a command name and cwd per tracked process. The `ppids` table
    // itself is created by SCHEMA (IF NOT EXISTS) on every open, but that can't add
    // columns to a db that already made the table, so ALTER them in here.
    let _ = conn.execute("ALTER TABLE ppids ADD COLUMN cmd TEXT", []);
    let _ = conn.execute("ALTER TABLE ppids ADD COLUMN cwd TEXT", []);
    // ev_commit was revised to key on the commit sha (see EV_COMMIT_SQL); DROP+CREATE
    // so an existing db running the old head-keyed trigger is upgraded in place.
    let _ = conn.execute("DROP TRIGGER IF EXISTS ev_commit", []);
    // Only advance the schema version once the commit trigger is actually
    // installed. Bumping it after a failed install would gate the migration out of
    // every future open, silently disabling `commit` feed events for this db.
    if conn.execute_batch(EV_COMMIT_SQL).is_ok() {
        let _ = conn.execute(&format!("PRAGMA user_version = {SCHEMA_VERSION}"), []);
    }
}

/// Whether the repo at `root` has changed since its last scan — by the root
/// directory's mtime — recording the new mtime when it has. Returns `true`
/// (rescan) if the root's mtime is newer than what was stored (or nothing was),
/// `false` (skip) otherwise. This is the cheap filter that lets the daemon skip
/// stat-walking a repo whose working tree hasn't been touched, instead of
/// re-checking every indexed repo on every pass.
pub fn root_mtime_advanced(conn: &Connection, git_dir: &Path, root: &Path) -> bool {
    let Some(cur) = root_mtime(root) else {
        return true; // can't stat the root → don't skip, scan to be safe
    };
    let stored: Option<i64> = conn
        .query_row("SELECT mtime FROM repos WHERE git_dir = ?1", [git_dir.to_string_lossy()], |r| r.get(0))
        .optional()
        .ok()
        .flatten();
    if stored.is_some_and(|s| cur <= s) {
        return false; // not newer → nothing changed here, skip
    }
    let _ = conn.execute(
        "UPDATE repos SET mtime = ?2 WHERE git_dir = ?1",
        rusqlite::params![git_dir.to_string_lossy(), cur],
    );
    true
}

/// The mtime (epoch seconds) of `root`'s directory inode, if statable — the cheap,
/// db-free primitive the statusd compute workers use to decide whether a repo's
/// root changed before doing the expensive `is_dirty` scan.
pub fn root_mtime(root: &Path) -> Option<i64> {
    std::fs::metadata(root)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
}

/// Persist a repo's scanned root mtime by ledger id (the statusd writer path, which
/// keys on id rather than git-dir).
pub fn set_repo_mtime_by_id(conn: &Connection, id: i64, mtime: i64) {
    let _ = conn.execute("UPDATE repos SET mtime = ?2 WHERE id = ?1", rusqlite::params![id, mtime]);
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
/// Returns the number pruned. Job history is preserved but *detached* (repo_id
/// nulled), so it can't rejoin a newly-indexed repo that reuses the rowid.
pub fn prune_missing(conn: &Connection) -> Result<usize> {
    let mut stmt = conn.prepare("SELECT id, git_dir FROM repos")?;
    let rows: Vec<(i64, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    let mut removed = 0;
    for (id, git_dir) in rows {
        if !std::path::Path::new(&git_dir).exists() {
            // Clear everything keyed by this repo_id first: SQLite reuses rowids and
            // FKs are off, so an orphaned row could otherwise silently reattach to a
            // newly-indexed repo that reuses this id. Jobs are DETACHED (repo_id →
            // NULL) rather than deleted — history survives, but `pending_failures`
            // (INNER JOIN) and `list_jobs` (LEFT JOIN, NULL git_dir) can't
            // mis-attribute a pruned repo's failure to the id's next occupant.
            conn.execute("UPDATE jobs SET repo_id=NULL WHERE repo_id=?1", [id])?;
            conn.execute("DELETE FROM claims WHERE repo_id=?1", [id])?;
            conn.execute("DELETE FROM repo_status WHERE repo_id=?1", [id])?;
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
         ON CONFLICT(git_dir) DO UPDATE SET last_seen = ?3, workdir = COALESCE(?2, workdir)",
        rusqlite::params![gd, wd, ts],
    )?;
    let id: i64 = conn.query_row(
        "SELECT id FROM repos WHERE git_dir = ?1",
        [gd],
        |r| r.get(0),
    )?;
    Ok(id)
}

/// Drop a repo from the index by git dir (used by `git zwatch rm`). Returns the
/// number of rows removed (0 if it was not indexed).
pub fn remove_repo(conn: &Connection, git_dir: &Path) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM repos WHERE git_dir = ?1",
        [git_dir.to_string_lossy()],
    )?)
}

/// Record (or clear, with `None`) the `zvcs.hook` command for a repo in the
/// index, so the daemon can find armed repos without opening every repo's config.
/// Upserts the repo row if it isn't indexed yet (arming implies watching it).
pub fn set_repo_hook(conn: &Connection, git_dir: &Path, workdir: Option<&Path>, hook: Option<&str>) -> Result<()> {
    upsert_repo(conn, git_dir, workdir)?;
    conn.execute(
        "UPDATE repos SET hook = ?2 WHERE git_dir = ?1",
        rusqlite::params![git_dir.to_string_lossy(), hook],
    )?;
    Ok(())
}

/// Every indexed repo that carries a hook — the armed set the daemon watches for
/// triggers. Returns `(git_dir, workdir)`; skips rows with a null/empty hook.
/// This is an indexed read, so a busy machine's daemon starts in O(armed), not
/// O(all indexed repos).
pub fn list_armed(conn: &Connection) -> Result<Vec<(String, Option<String>)>> {
    let mut stmt = conn.prepare(
        "SELECT git_dir, workdir FROM repos WHERE hook IS NOT NULL AND trim(hook) <> ''",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)))?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// Directory triggers (`git ztrigger <DIR> <command>`) — a general "watch this
/// directory, run this command on any change" mechanism, independent of git. The
/// key is the canonical directory path; setting the same path again replaces the
/// command.
pub fn set_trigger(conn: &Connection, path: &Path, command: &str, throttle_ms: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO triggers (path, command, created_at, throttle_ms) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(path) DO UPDATE SET command = ?2, throttle_ms = ?4",
        rusqlite::params![path.to_string_lossy(), command, now(), throttle_ms],
    )?;
    Ok(())
}

/// Remove a directory trigger. Returns the number of rows removed (0 if absent).
pub fn remove_trigger(conn: &Connection, path: &Path) -> Result<usize> {
    Ok(conn.execute("DELETE FROM triggers WHERE path = ?1", [path.to_string_lossy()])?)
}

/// One directory trigger.
pub struct TriggerRow {
    pub path: String,
    pub command: String,
    /// Leading-edge throttle: after a fire, suppress further fires for this many
    /// milliseconds (0 = fire on every event).
    pub throttle_ms: i64,
}

/// Every directory trigger. The watch set the daemon builds for `ztrigger`.
pub fn list_triggers(conn: &Connection) -> Result<Vec<TriggerRow>> {
    let mut stmt = conn.prepare("SELECT path, command, throttle_ms FROM triggers ORDER BY path")?;
    let rows = stmt.query_map([], |r| {
        Ok(TriggerRow { path: r.get(0)?, command: r.get(1)?, throttle_ms: r.get(2)? })
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// Whether any directory trigger exists — a cheap check the daemon/autostart use
/// to decide whether to watch even when no `[zvcs]` autonomy/hook/status is set.
pub fn has_triggers() -> bool {
    open_ro()
        .ok()
        .and_then(|c| c.query_row("SELECT EXISTS(SELECT 1 FROM triggers)", [], |r| r.get::<_, i64>(0)).ok())
        .map(|n| n != 0)
        .unwrap_or(false)
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
    repo_id: Option<i64>,
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

/// Atomically claim a job for running: transition `queued` → `running`. Returns
/// false if the row was not `queued` (e.g. a `zjob stop` already set `stopped`),
/// so the worker can bail instead of running a cancelled job.
pub fn claim_running(conn: &Connection, id: i64) -> Result<bool> {
    let n = conn.execute(
        "UPDATE jobs SET state='running', started_at=?2 WHERE id=?1 AND state='queued'",
        rusqlite::params![id, now()],
    )?;
    Ok(n > 0)
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

/// Prune terminal jobs (`done`/`failed`/`stopped`) whose `finished_at` is older
/// than `retention_secs` ago, so the ledger doesn't grow without bound across
/// millions of commands. In-flight jobs (`queued`/`running`) and rows with a NULL
/// `finished_at` are never touched. Returns the number of rows deleted. The daemon
/// calls this on a slow timer; SQLite reuses the freed pages, so no explicit
/// `VACUUM` (an exclusive-lock, whole-db rewrite) is run under fleet load.
pub fn prune_stale_jobs(conn: &Connection, retention_secs: i64) -> Result<usize> {
    let cutoff = now() - retention_secs;
    let n = conn.execute(
        "DELETE FROM jobs
           WHERE state IN ('done','failed','stopped')
             AND finished_at IS NOT NULL
             AND finished_at < ?1",
        rusqlite::params![cutoff],
    )?;
    Ok(n)
}

/// Prune feed events older than the retention window, so the `events` table stays
/// bounded like the job ledger. Returns rows deleted.
pub fn prune_stale_events(conn: &Connection, retention_secs: i64) -> Result<usize> {
    let cutoff = now() - retention_secs;
    let n = conn.execute("DELETE FROM events WHERE ts < ?1", rusqlite::params![cutoff])?;
    Ok(n)
}

/// Prune inter-agent messages older than the retention window, and any
/// `message_reads` rows left orphaned by that delete, so the `messages` tables
/// stay bounded like the job ledger and the event feed. Returns messages deleted.
pub fn prune_stale_messages(conn: &Connection, retention_secs: i64) -> Result<usize> {
    let cutoff = now() - retention_secs;
    let n = conn.execute("DELETE FROM messages WHERE ts < ?1", rusqlite::params![cutoff])?;
    let _ = conn.execute(
        "DELETE FROM message_reads WHERE message_id NOT IN (SELECT id FROM messages)",
        [],
    );
    Ok(n)
}

/// One row of the unified event feed (`git zevents`/`ztail`), resolved to its
/// repo path for display. `commit`/`status` events come from the `repo_status`
/// triggers; `reconcile` events are inserted by the daemon's reconcile path.
pub struct EventRow {
    pub id: i64,
    pub ts: i64,
    pub kind: String,
    pub git_dir: Option<String>,
    pub workdir: Option<String>,
    pub detail: Option<String>,
    pub sha_before: Option<String>,
    pub sha_after: Option<String>,
}

/// Append one event to the unified feed. Used for `reconcile` events the
/// `repo_status` triggers cannot observe; `commit`/`status` events are inserted
/// by those triggers.
pub fn record_event(
    conn: &Connection,
    kind: &str,
    repo_id: Option<i64>,
    detail: Option<&str>,
    sha_before: Option<&str>,
    sha_after: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO events (ts, kind, repo_id, detail, sha_before, sha_after)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![now(), kind, repo_id, detail, sha_before, sha_after],
    )?;
    Ok(())
}

/// Highest event id (0 if empty) — the follow cursor's starting point so a live
/// tail only prints events that arrive after it attaches.
pub fn max_event_id(conn: &Connection) -> i64 {
    conn.query_row("SELECT COALESCE(MAX(id), 0) FROM events", [], |r| r.get(0)).unwrap_or(0)
}

// Shared SELECT for both feed queries; `%` filters bind as NULL to disable.
const EVENT_SELECT: &str = "SELECT e.id, e.ts, e.kind, r.git_dir, r.workdir, e.detail, e.sha_before, e.sha_after \
     FROM events e LEFT JOIN repos r ON r.id = e.repo_id \
    WHERE e.id > ?1 \
      AND (?2 IS NULL OR e.kind = ?2) \
      AND (?3 IS NULL OR r.git_dir LIKE ?3 OR r.workdir LIKE ?3)";

fn row_to_event(r: &rusqlite::Row) -> rusqlite::Result<EventRow> {
    Ok(EventRow {
        id: r.get(0)?,
        ts: r.get(1)?,
        kind: r.get(2)?,
        git_dir: r.get(3)?,
        workdir: r.get(4)?,
        detail: r.get(5)?,
        sha_before: r.get(6)?,
        sha_after: r.get(7)?,
    })
}

/// The most recent `limit` events (ascending order for display) — the backlog a
/// live tail prints before it starts following. Optional kind / repo-substring filters.
pub fn events_recent(
    conn: &Connection,
    limit: usize,
    kind: Option<&str>,
    repo_like: Option<&str>,
) -> Result<Vec<EventRow>> {
    let like = repo_like.map(|p| format!("%{p}%"));
    let sql = format!("{EVENT_SELECT} ORDER BY e.id DESC LIMIT ?4");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(rusqlite::params![0i64, kind, like, limit as i64], row_to_event)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows.into_iter().rev().collect()) // newest-first query → oldest-first display
}

/// Events with id greater than `after_id`, oldest first — the follow poll.
pub fn events_since(
    conn: &Connection,
    after_id: i64,
    kind: Option<&str>,
    repo_like: Option<&str>,
) -> Result<Vec<EventRow>> {
    let like = repo_like.map(|p| format!("%{p}%"));
    let sql = format!("{EVENT_SELECT} ORDER BY e.id ASC LIMIT 500");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(rusqlite::params![after_id, kind, like], row_to_event)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
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
    #[allow(clippy::type_complexity)] // one-off row tuple; a named alias adds no clarity
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
    head_sha: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO repo_status (repo_id, dirty, detached, sync, head, head_sha, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(repo_id) DO UPDATE SET
             dirty=?2, detached=?3, sync=?4, head=?5, head_sha=?6, updated_at=?7",
        rusqlite::params![repo_id, dirty as i64, detached as i64, sync, head, head_sha, now()],
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
    // One transaction: the DELETE of the prior snapshot and the N INSERTs must be
    // all-or-nothing. Otherwise a crash mid-write would drop the previous good
    // snapshot AND leave a partial one, so `zrestore` would half-restore the tree
    // while reporting success (and a concurrent reader could observe the gap).
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM snapshots WHERE name = ?1", [name])?;
    let ts = now();
    for (git_dir, workdir, sha) in entries {
        tx.execute(
            "INSERT INTO snapshots (name, git_dir, workdir, sha, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![name, git_dir, workdir, sha, ts],
        )?;
    }
    tx.commit()?;
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

/// Clear any existing stash records for `name` (start a fresh tree-wide stash).
pub fn stash_begin(conn: &Connection, name: &str) -> Result<()> {
    conn.execute("DELETE FROM stashes WHERE name=?1", [name])?;
    Ok(())
}

/// Record that `workdir` was stashed under `name`.
pub fn stash_add(conn: &Connection, name: &str, git_dir: &str, workdir: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO stashes (name, git_dir, workdir, created_at) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![name, git_dir, workdir, now()],
    )?;
    Ok(())
}

/// The workdirs stashed under `name` (most-recently-added first, so pop is LIFO).
pub fn stash_entries(conn: &Connection, name: &str) -> Result<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT workdir FROM stashes WHERE name=?1 ORDER BY rowid DESC")?;
    let rows = stmt
        .query_map([name], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Remove one repo's stash record under `name`.
pub fn stash_remove_entry(conn: &Connection, name: &str, workdir: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM stashes WHERE name=?1 AND workdir=?2",
        rusqlite::params![name, workdir],
    )?;
    Ok(())
}

/// List stash names with their repo counts.
pub fn list_stashes(conn: &Connection) -> Result<Vec<(String, i64)>> {
    let mut stmt = conn.prepare("SELECT name, COUNT(*) FROM stashes GROUP BY name ORDER BY name")?;
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

/// Release `repo_id`'s claim regardless of which session holds it — the escape
/// hatch for a lease left behind by a dead agent (no TTL/expiry otherwise).
/// Returns true if a claim was removed.
pub fn unclaim_force(conn: &Connection, repo_id: i64) -> Result<bool> {
    let n = conn.execute("DELETE FROM claims WHERE repo_id=?1", [repo_id])?;
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

// ---- zpin: per-repo autonomy freeze ----------------------------------------

/// Set or clear a repo's pinned flag. A pinned repo is refused by autobump and
/// reconcile in the daemon — a selective freeze on autonomy.
pub fn set_pin(conn: &Connection, git_dir: &Path, pinned: bool) -> Result<()> {
    conn.execute(
        "UPDATE repos SET pinned=?2 WHERE git_dir=?1",
        rusqlite::params![git_dir.to_string_lossy(), pinned as i64],
    )?;
    Ok(())
}

/// Whether a repo is pinned. Unknown repo → false.
pub fn is_pinned(conn: &Connection, git_dir: &Path) -> bool {
    conn.query_row(
        "SELECT COALESCE(pinned,0) FROM repos WHERE git_dir=?1",
        [git_dir.to_string_lossy()],
        |r| r.get::<_, i64>(0),
    )
    .map(|v| v != 0)
    .unwrap_or(false)
}

/// Every pinned repo's `(git_dir, workdir)`.
pub fn list_pins(conn: &Connection) -> Result<Vec<(String, Option<String>)>> {
    let mut stmt = conn.prepare("SELECT git_dir, workdir FROM repos WHERE pinned=1 ORDER BY git_dir")?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

// ---- zbroadcast/zhandoff: inter-agent messages -----------------------------

/// Post a message. `to` None = broadcast to every session. Returns the id.
pub fn post_message(conn: &Connection, from: &str, to: Option<&str>, body: &str) -> Result<i64> {
    conn.execute(
        "INSERT INTO messages (ts, from_session, to_session, body) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![now(), from, to, body],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Messages `session` has not read (broadcast or addressed to it, not its own),
/// oldest first: `(id, from_session, body, ts)`.
#[allow(clippy::type_complexity)] // one-off row tuple return; a named alias adds no clarity
pub fn unread_messages(
    conn: &Connection,
    session: &str,
) -> Result<Vec<(i64, Option<String>, String, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT id, from_session, body, ts FROM messages m
          WHERE (to_session IS NULL OR to_session = ?1)
            AND from_session IS NOT ?1
            AND NOT EXISTS (SELECT 1 FROM message_reads r WHERE r.message_id=m.id AND r.session=?1)
          ORDER BY id ASC",
    )?;
    let rows = stmt
        .query_map([session], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Mark messages read by `session` (idempotent).
pub fn mark_read(conn: &Connection, ids: &[i64], session: &str) -> Result<()> {
    for id in ids {
        conn.execute(
            "INSERT OR IGNORE INTO message_reads (message_id, session) VALUES (?1, ?2)",
            rusqlite::params![id, session],
        )?;
    }
    Ok(())
}

/// Reassign a repo's claim to `new_session`. False if the repo isn't claimed.
pub fn transfer_claim(conn: &Connection, repo_id: i64, new_session: &str) -> Result<bool> {
    Ok(conn.execute("UPDATE claims SET session=?2 WHERE repo_id=?1", rusqlite::params![repo_id, new_session])? > 0)
}

// ---- zon: semantic-event subscriptions -------------------------------------

/// Add an event subscription: run `command` when a feed event matches `kind`
/// (None = any) and `repo_like` (None = any repo). Returns the id.
pub fn add_subscription(
    conn: &Connection,
    kind: Option<&str>,
    repo_like: Option<&str>,
    command: &str,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO subscriptions (kind, repo_like, command, created_at) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![kind, repo_like, command, now()],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Every subscription: `(id, kind, repo_like, command)`.
#[allow(clippy::type_complexity)] // one-off row tuple return; a named alias adds no clarity
pub fn list_subscriptions(
    conn: &Connection,
) -> Result<Vec<(i64, Option<String>, Option<String>, String)>> {
    let mut stmt = conn.prepare("SELECT id, kind, repo_like, command FROM subscriptions ORDER BY id")?;
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Remove a subscription by id. False if it didn't exist.
pub fn remove_subscription(conn: &Connection, id: i64) -> Result<bool> {
    Ok(conn.execute("DELETE FROM subscriptions WHERE id=?1", [id])? > 0)
}

/// Feed events at or after epoch `ts`, oldest first, with the same repo-path join
/// and optional kind/repo filters as the other feed queries. Backs `zsince`.
pub fn events_after_ts(
    conn: &Connection,
    ts: i64,
    kind: Option<&str>,
    repo_like: Option<&str>,
) -> Result<Vec<EventRow>> {
    let like = repo_like.map(|p| format!("%{p}%"));
    let mut stmt = conn.prepare(
        "SELECT e.id, e.ts, e.kind, r.git_dir, r.workdir, e.detail, e.sha_before, e.sha_after \
           FROM events e LEFT JOIN repos r ON r.id = e.repo_id \
          WHERE e.ts >= ?1 \
            AND (?2 IS NULL OR e.kind = ?2) \
            AND (?3 IS NULL OR r.git_dir LIKE ?3 OR r.workdir LIKE ?3) \
          ORDER BY e.id ASC",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![ts, kind, like], row_to_event)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Earliest creation time of a snapshot by `name`, if any (a `zsince` baseline).
pub fn snapshot_created_at(conn: &Connection, name: &str) -> Result<Option<i64>> {
    Ok(conn
        .query_row("SELECT MIN(created_at) FROM snapshots WHERE name=?1", [name], |r| {
            r.get::<_, Option<i64>>(0)
        })
        .optional()?
        .flatten())
}

/// Per-repo pending job pressure for `zcontend`: `(repo_path, queued, running)`
/// for every repo that currently has queued or running jobs, busiest first.
pub fn contention(conn: &Connection) -> Result<Vec<(String, i64, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT COALESCE(r.workdir, r.git_dir),
                SUM(j.state='queued'), SUM(j.state='running')
           FROM jobs j JOIN repos r ON r.id = j.repo_id
          WHERE j.state IN ('queued','running')
          GROUP BY j.repo_id
          ORDER BY (SUM(j.state='queued') + SUM(j.state='running')) DESC",
    )?;
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Every indexed repo's `(git_dir, workdir)` — the raw set for `zgraph` topology.
pub fn all_repos(conn: &Connection) -> Result<Vec<(String, Option<String>)>> {
    let mut stmt = conn.prepare("SELECT git_dir, workdir FROM repos ORDER BY workdir")?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Every repo's cached status for `zwaitfor`/`zcontend`: `(workdir_or_git_dir,
/// dirty, sync, head_sha)`.
pub fn all_status(conn: &Connection) -> Result<Vec<(String, bool, String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT COALESCE(r.workdir, r.git_dir), COALESCE(s.dirty,0), COALESCE(s.sync,''), COALESCE(s.head_sha,'')
           FROM repo_status s JOIN repos r ON r.id = s.repo_id",
    )?;
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? != 0, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

// ---- zppid: per-process commit productivity -------------------------------

/// One process row: the responsible process's pid, its command name and cwd, and
/// its accumulated commit tally with first/last activity timestamps.
pub struct PpidRow {
    pub session: String,
    pub ppid: i64,
    pub cmd: String,
    pub cwd: String,
    pub commits: i64,
    pub first_seen: i64,
    pub last_seen: i64,
}

/// Credit one commit to `session`, upserting the process row. `ppid`/`cmd`/`cwd`
/// (the durable responsible process, its command name, and its working directory)
/// are refreshed on every commit. Called only when a commit-producing verb advanced
/// HEAD, so the count reflects real commits, not attempts.
pub fn ppid_record_commit(conn: &Connection, session: &str, ppid: i64, cmd: &str, cwd: &str) -> Result<()> {
    let ts = now();
    conn.execute(
        "INSERT INTO ppids (session, ppid, cmd, cwd, commits, first_seen, last_seen)
         VALUES (?1, ?2, ?3, ?4, 1, ?5, ?5)
         ON CONFLICT(session) DO UPDATE SET
             commits = commits + 1, ppid = ?2, cmd = ?3, cwd = ?4, last_seen = ?5",
        rusqlite::params![session, ppid, cmd, cwd, ts],
    )?;
    Ok(())
}

/// Register (or refresh) a process's identity WITHOUT bumping its commit count — the
/// path for a non-commit mutating verb, so a process that only pushes/adds still
/// appears in `ppids` (with its pid/cmd/cwd) and can carry a `proc_verbs` breakdown.
pub fn ppid_touch(conn: &Connection, session: &str, ppid: i64, cmd: &str, cwd: &str) -> Result<()> {
    let ts = now();
    conn.execute(
        "INSERT INTO ppids (session, ppid, cmd, cwd, commits, first_seen, last_seen)
         VALUES (?1, ?2, ?3, ?4, 0, ?5, ?5)
         ON CONFLICT(session) DO UPDATE SET ppid = ?2, cmd = ?3, cwd = ?4, last_seen = ?5",
        rusqlite::params![session, ppid, cmd, cwd, ts],
    )?;
    Ok(())
}

/// Increment a process's per-verb command count (`proc_verbs`), upserting the row.
pub fn proc_verb_record(conn: &Connection, session: &str, verb: &str) -> Result<()> {
    let ts = now();
    conn.execute(
        "INSERT INTO proc_verbs (session, verb, count, first_seen, last_seen)
         VALUES (?1, ?2, 1, ?3, ?3)
         ON CONFLICT(session, verb) DO UPDATE SET count = count + 1, last_seen = ?3",
        rusqlite::params![session, verb, ts],
    )?;
    Ok(())
}

/// One per-verb tally row for `git zprocs`.
pub struct ProcVerbRow {
    pub session: String,
    pub verb: String,
    pub count: i64,
    pub last_seen: i64,
}

/// Every per-verb tally, busiest process/verb first — joined per session against
/// `ppids` by the caller for the process's pid/cmd/cwd.
pub fn list_proc_verbs(conn: &Connection) -> Result<Vec<ProcVerbRow>> {
    let mut stmt = conn.prepare(
        "SELECT session, verb, count, COALESCE(last_seen,0)
           FROM proc_verbs ORDER BY count DESC",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(ProcVerbRow {
                session: r.get(0)?,
                verb: r.get(1)?,
                count: r.get(2)?,
                last_seen: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Every tracked process, most productive first (ties broken by most-recent
/// activity) — the full table behind `git zppid` and the dashboard's summary.
pub fn list_ppids(conn: &Connection) -> Result<Vec<PpidRow>> {
    let mut stmt = conn.prepare(
        "SELECT session, COALESCE(ppid,0), COALESCE(cmd,''), COALESCE(cwd,''),
                commits, COALESCE(first_seen,0), COALESCE(last_seen,0)
           FROM ppids ORDER BY commits DESC, last_seen DESC",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(PpidRow {
                session: r.get(0)?,
                ppid: r.get(1)?,
                cmd: r.get(2)?,
                cwd: r.get(3)?,
                commits: r.get(4)?,
                first_seen: r.get(5)?,
                last_seen: r.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

#[cfg(test)]
mod snapshot_atomic_tests {
    use super::{
        add_subscription, claim, events_recent, events_since, insert_job, is_pinned, job_finished,
        list_claims, list_pins, list_subscriptions, load_snapshot, mark_read, post_message,
        prune_stale_jobs, prune_stale_messages, record_event, remove_subscription,
        root_mtime_advanced, save_snapshot,
        set_pin, transfer_claim, unread_messages, upsert_repo, upsert_status, EV_COMMIT_SQL, SCHEMA,
    };
    use rusqlite::Connection;
    use std::path::Path;

    #[test]
    fn save_snapshot_rolls_back_on_mid_write_failure() {
        // Prove the DELETE + N INSERTs are one transaction: if an INSERT fails
        // part-way, the prior good snapshot must survive intact (the pre-fix code
        // autocommitted the DELETE first, destroying it).
        let dir = std::env::temp_dir().join(format!("zvcs-snapatom-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let conn = Connection::open(dir.join("t.sqlite")).unwrap();
        conn.execute_batch(SCHEMA).unwrap();

        let good = vec![
            ("g1".into(), "w1".into(), "s1".into()),
            ("g2".into(), "w2".into(), "s2".into()),
            ("g3".into(), "w3".into(), "s3".into()),
        ];
        save_snapshot(&conn, "snap", &good).unwrap();
        assert_eq!(load_snapshot(&conn, "snap").unwrap().len(), 3);

        // Make the 2nd INSERT of a re-save abort mid-transaction.
        conn.execute_batch(
            "CREATE TRIGGER boom BEFORE INSERT ON snapshots \
             WHEN NEW.sha = 'BOOM' BEGIN SELECT RAISE(ABORT, 'boom'); END;",
        )
        .unwrap();

        let doomed: Vec<(String, String, String)> = vec![
            ("n1".into(), "w".into(), "ok".into()),
            ("n2".into(), "w".into(), "BOOM".into()),
        ];
        assert!(save_snapshot(&conn, "snap", &doomed).is_err(), "the poisoned INSERT must fail the save");

        // Rolled back: the ORIGINAL 3-entry snapshot is intact, not replaced by the
        // partial (1-entry) doomed one.
        let after = load_snapshot(&conn, "snap").unwrap();
        assert_eq!(after.len(), 3, "failed save must not destroy the prior snapshot (got {})", after.len());
        assert!(after.iter().any(|(g, _, _)| g == "g1"), "original entries must survive");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn root_mtime_advanced_gates_rescans() {
        let dir = std::env::temp_dir().join(format!("zvcs-mtime-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let conn = Connection::open(dir.join("t.sqlite")).unwrap();
        conn.execute_batch(SCHEMA).unwrap();

        let root = dir.join("repo");
        let gd = root.join(".git");
        std::fs::create_dir_all(&root).unwrap();
        upsert_repo(&conn, &gd, Some(&root)).unwrap();

        // First look: never scanned → must scan, and it records the mtime.
        assert!(root_mtime_advanced(&conn, &gd, &root), "first scan must run");
        // Immediately again, root untouched → skip.
        assert!(!root_mtime_advanced(&conn, &gd, &root), "unchanged root must be skipped");
        // Simulate a newer root mtime by rolling the STORED value back (mtime is
        // second-granular, so we can't reliably touch-and-advance within a test).
        conn.execute("UPDATE repos SET mtime = mtime - 10 WHERE git_dir = ?1", [gd.to_string_lossy()]).unwrap();
        assert!(root_mtime_advanced(&conn, &gd, &root), "a newer root mtime must re-scan");
        // And once recorded again, it skips.
        assert!(!root_mtime_advanced(&conn, &gd, &root), "must skip after re-recording");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pins_messages_subscriptions_roundtrip() {
        let dir = std::env::temp_dir().join(format!("zvcs-coord-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let conn = Connection::open(dir.join("t.sqlite")).unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        let id = upsert_repo(&conn, Path::new("/x/repo/.git"), Some(Path::new("/x/repo"))).unwrap();

        // zpin
        assert!(!is_pinned(&conn, Path::new("/x/repo/.git")));
        set_pin(&conn, Path::new("/x/repo/.git"), true).unwrap();
        assert!(is_pinned(&conn, Path::new("/x/repo/.git")));
        assert_eq!(list_pins(&conn).unwrap().len(), 1);
        set_pin(&conn, Path::new("/x/repo/.git"), false).unwrap();
        assert!(!is_pinned(&conn, Path::new("/x/repo/.git")));

        // zbroadcast: broadcast reaches everyone but the sender; targeted reaches one.
        post_message(&conn, "alice", None, "hi all").unwrap();
        post_message(&conn, "alice", Some("bob"), "hi bob").unwrap();
        assert_eq!(unread_messages(&conn, "bob").unwrap().len(), 2, "bob: broadcast + targeted");
        assert_eq!(unread_messages(&conn, "carol").unwrap().len(), 1, "carol: broadcast only");
        assert_eq!(unread_messages(&conn, "alice").unwrap().len(), 0, "sender never gets own");
        let ids: Vec<i64> = unread_messages(&conn, "bob").unwrap().iter().map(|(i, ..)| *i).collect();
        mark_read(&conn, &ids, "bob").unwrap();
        assert_eq!(unread_messages(&conn, "bob").unwrap().len(), 0, "read is per-session");
        assert_eq!(unread_messages(&conn, "carol").unwrap().len(), 1, "bob's read doesn't touch carol");

        // zon subscriptions
        let sid = add_subscription(&conn, Some("commit"), Some("foo"), "echo hi").unwrap();
        assert_eq!(list_subscriptions(&conn).unwrap().len(), 1);
        assert!(remove_subscription(&conn, sid).unwrap());
        assert!(!remove_subscription(&conn, sid).unwrap(), "removing twice is a no-op");

        // zhandoff: transfer a claim to another session.
        let _ = claim(&conn, id, "alice", Some("/x/repo")).unwrap();
        assert!(transfer_claim(&conn, id, "bob").unwrap());
        assert_eq!(list_claims(&conn).unwrap()[0].1, "bob", "claim reassigned");
        assert!(!transfer_claim(&conn, 999, "bob").unwrap(), "unknown repo → false");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_stale_messages_drops_aged_rows_and_orphaned_reads() {
        let dir = std::env::temp_dir().join(format!("zvcs-msgprune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let conn = Connection::open(dir.join("t.sqlite")).unwrap();
        conn.execute_batch(SCHEMA).unwrap();

        post_message(&conn, "a", None, "old").unwrap(); // id 1
        post_message(&conn, "a", None, "new").unwrap(); // id 2
        mark_read(&conn, &[1, 2], "b").unwrap(); // b read both

        let week = 7 * 24 * 3600i64;
        conn.execute("UPDATE messages SET ts = ts - ?1 WHERE id = 1", [week + 60]).unwrap();
        assert_eq!(prune_stale_messages(&conn, week).unwrap(), 1, "only the aged message is dropped");

        let msgs: i64 = conn.query_row("SELECT count(*) FROM messages", [], |r| r.get(0)).unwrap();
        assert_eq!(msgs, 1, "the fresh message survives");
        let reads: i64 = conn.query_row("SELECT count(*) FROM message_reads", [], |r| r.get(0)).unwrap();
        assert_eq!(reads, 1, "the pruned message's read row is cleaned; the survivor's stays");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_stale_jobs_deletes_only_old_terminal_rows() {
        let dir = std::env::temp_dir().join(format!("zvcs-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let conn = Connection::open(dir.join("t.sqlite")).unwrap();
        conn.execute_batch(SCHEMA).unwrap();

        // Three finished (done) jobs + one still queued.
        let old1 = insert_job(&conn, None, "exec", "{}", None).unwrap();
        let old2 = insert_job(&conn, None, "exec", "{}", None).unwrap();
        let recent = insert_job(&conn, None, "exec", "{}", None).unwrap();
        let queued = insert_job(&conn, None, "exec", "{}", None).unwrap();
        for id in [old1, old2, recent] {
            job_finished(&conn, id, "done", 0, "", None).unwrap();
        }

        // Age two of the finished rows past the 7-day window; leave `recent` fresh.
        let week = 7 * 24 * 3600i64;
        conn.execute(
            "UPDATE jobs SET finished_at = finished_at - ?1 WHERE id IN (?2, ?3)",
            rusqlite::params![week + 60, old1, old2],
        )
        .unwrap();

        let deleted = prune_stale_jobs(&conn, week).unwrap();
        assert_eq!(deleted, 2, "only the two aged terminal rows may be pruned");

        let survivors: Vec<i64> = conn
            .prepare("SELECT id FROM jobs ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        // The fresh `done` job and the still-`queued` job (never eligible) survive.
        assert_eq!(survivors, vec![recent, queued], "fresh + in-flight jobs must be kept");

        // A second prune with nothing aged removes nothing.
        assert_eq!(prune_stale_jobs(&conn, week).unwrap(), 0, "idempotent once nothing is stale");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn events_feed_via_status_triggers() {
        let dir = std::env::temp_dir().join(format!("zvcs-events-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let conn = Connection::open(dir.join("t.sqlite")).unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn.execute_batch(EV_COMMIT_SQL).unwrap(); // open_rw installs this; mirror it here
        let id = upsert_repo(&conn, Path::new("/x/repo/.git"), Some(Path::new("/x/repo"))).unwrap();

        // First status row is an INSERT — the triggers fire on UPDATE only, so a
        // repo joining the index must NOT flood the feed with a bogus event.
        upsert_status(&conn, id, false, false, "up-to-date", "main", "aaaaaaaaaa00").unwrap();
        assert_eq!(events_recent(&conn, 10, None, None).unwrap().len(), 0, "cold-start INSERT emits nothing");

        // The commit SHA moves while the branch name (`head`) stays "main" — the
        // exact case the original head-keyed trigger missed. Must emit one `commit`.
        upsert_status(&conn, id, false, false, "up-to-date", "main", "bbbbbbbbbb11").unwrap();
        // Goes dirty → one `status` event; sync change → another.
        upsert_status(&conn, id, true, false, "up-to-date", "main", "bbbbbbbbbb11").unwrap();
        upsert_status(&conn, id, true, false, "ahead", "main", "bbbbbbbbbb11").unwrap();
        // A reconcile is recorded directly (triggers can't see it).
        record_event(&conn, "reconcile", Some(id), Some("fast-forwarded"), None, None).unwrap();

        let evs = events_recent(&conn, 10, None, None).unwrap();
        assert_eq!(evs.len(), 4, "commit + 2 status + reconcile");
        assert_eq!(evs[0].kind, "commit");
        assert_eq!(evs[0].sha_before.as_deref(), Some("aaaaaaaaaa00"));
        assert_eq!(evs[0].sha_after.as_deref(), Some("bbbbbbbbbb11"));
        assert_eq!(evs[3].kind, "reconcile");
        assert_eq!(evs[0].workdir.as_deref(), Some("/x/repo"), "path resolved via JOIN");

        // Filters + incremental follow cursor.
        assert_eq!(events_recent(&conn, 10, Some("status"), None).unwrap().len(), 2, "kind filter");
        assert_eq!(events_recent(&conn, 10, None, Some("repo")).unwrap().len(), 4, "repo substring filter");
        assert_eq!(events_recent(&conn, 10, None, Some("nomatch")).unwrap().len(), 0);
        let after_first = events_since(&conn, evs[0].id, None, None).unwrap();
        assert_eq!(after_first.len(), 3, "follow only sees events past the cursor");

        // A migrated row (head_sha starts NULL) must not emit a phantom commit when
        // first backfilled to a real sha; a later real move still fires.
        let id2 = upsert_repo(&conn, Path::new("/x/other/.git"), Some(Path::new("/x/other"))).unwrap();
        conn.execute(
            "INSERT INTO repo_status(repo_id,dirty,detached,sync,head,head_sha) VALUES(?1,0,0,'up-to-date','main',NULL)",
            [id2],
        )
        .unwrap();
        let commits = events_recent(&conn, 50, Some("commit"), None).unwrap().len();
        upsert_status(&conn, id2, false, false, "up-to-date", "main", "dddddddddd33").unwrap();
        assert_eq!(
            events_recent(&conn, 50, Some("commit"), None).unwrap().len(),
            commits,
            "NULL->sha backfill emits no phantom commit"
        );
        upsert_status(&conn, id2, false, false, "up-to-date", "main", "eeeeeeeeee44").unwrap();
        assert_eq!(
            events_recent(&conn, 50, Some("commit"), None).unwrap().len(),
            commits + 1,
            "a real sha move after backfill fires"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
