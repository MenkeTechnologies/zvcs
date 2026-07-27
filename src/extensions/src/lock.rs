//! Client for the per-repo `zdaemon` coordinator — zvcs's fair replacement for
//! git's `index.lock`.
//!
//! Stock git guards index writes with an `O_EXCL` lockfile: a contended writer
//! does not wait, it *fails* (`Unable to create '.git/index.lock'`). Under many
//! concurrent agents that is a retry storm with no fairness. zvcs instead routes
//! every index-mutating operation through [`RepoLock::acquire`], which blocks in
//! the daemon's FIFO queue and returns only when the caller holds the lock — so
//! N processes serialize first-come-first-served instead of racing.
//!
//! The guard is RAII: dropping it (normal return, `?`, or panic) sends `RELEASE`
//! and closes the socket, and the daemon also auto-releases on socket EOF, so a
//! crashed holder can never wedge the repo.
//!
//! If no daemon is reachable the lock degrades to a **no-op guard**: the
//! operation still runs (exactly stock-git behavior, minus the fair queue).
//! Ensuring a daemon is running is the autonomous layer's job, not the writer's.

use std::cell::RefCell;
use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Per-process monotonic counter making every lock id unique even when one
/// process acquires from multiple threads (the daemon keys holder/release on the
/// id, so two live acquisitions must never share one).
static SEQ: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// Canonical git-dir keys this thread currently holds. A nested acquire of the
    /// same repo on the same thread returns a reentrant no-op guard instead of
    /// blocking in the daemon's FIFO forever: the outer guard can't drop while this
    /// thread is blocked, so the daemon would never promote the nested waiter
    /// (self-deadlock). Reentrancy is per-thread — a *different* thread (or process)
    /// acquiring the same repo still blocks and serializes via the daemon.
    static HELD: RefCell<HashSet<PathBuf>> = RefCell::new(HashSet::new());
}

/// Held index lock. While alive, this process is the sole writer the daemon has
/// granted for the repo. Release is automatic on drop.
#[must_use = "the lock releases when this guard is dropped; bind it for the critical section"]
pub struct RepoLock {
    /// `Some` when held via a live daemon; `None` for the no-daemon no-op guard.
    stream: Option<UnixStream>,
    id: String,
    /// The canonical lane key this guard registered in the thread-local `HELD` set,
    /// to clear on drop. `None` for a *reentrant* guard (an outer guard on this
    /// thread already owns the key, and will clear it).
    held_key: Option<PathBuf>,
    /// Makes `RepoLock` `!Send`: the reentrancy set is thread-local, so a guard
    /// moved to another thread would strand its key (and a later same-repo acquire
    /// on the origin thread would get a no-op guard with NO exclusion). Binding the
    /// guard to its acquiring thread turns that footgun into a compile error.
    _not_send: std::marker::PhantomData<*const ()>,
}

/// Outcome of [`RepoLock::try_acquire`] — the non-blocking lock.
pub enum TryLock {
    /// The lock was taken; hold the guard for the critical section.
    Held(RepoLock),
    /// The lane is contended (another writer holds it); the caller should queue
    /// its work as a job rather than block.
    Busy {
        /// Whether the daemon told us WHO holds the lane. `false` means the
        /// coordinator predates the `HOLDER` query, so we could not rule out that
        /// the holder is this process's own ancestor — and a caller that must not
        /// queue therefore cannot safely block either.
        owner_resolved: bool,
    },
    /// No daemon reachable — proceed unserialized, exactly as [`RepoLock::acquire`].
    NoDaemon,
}

impl RepoLock {
    /// Acquire the repo-wide index lock via the daemon at `<git_dir>/zvcs.sock`.
    ///
    /// Blocks in the daemon's fair FIFO until granted. Never fails: if the daemon
    /// is unreachable or the handshake does not complete, returns an unlocked
    /// no-op guard so the caller proceeds unserialized rather than erroring.
    ///
    /// The client id is generated internally and is unique per acquisition
    /// (`<pid>-<seq>`), so concurrent holders — across processes or threads —
    /// are never conflated by the daemon.
    pub fn acquire(git_dir: &Path) -> Self {
        let id = format!(
            "{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        );
        let sock = crate::superset::zdaemon::socket_path();
        let repo = lane_key(git_dir);

        // Reentrancy: if this thread already holds this repo, hand back a no-op
        // guard. A nested acquire that went to the daemon would queue behind the
        // outer hold and block forever — the outer guard can't drop while this
        // same thread is blocked in `read_line` (self-deadlock). A *different*
        // thread/process still goes to the daemon and serializes normally.
        if HELD.with(|h| h.borrow().contains(&repo)) {
            return Self { stream: None, id, held_key: None, _not_send: std::marker::PhantomData };
        }
        HELD.with(|h| {
            h.borrow_mut().insert(repo.clone());
        });

        let mut stream = match UnixStream::connect(&sock) {
            Ok(s) => s,
            Err(_) => return Self::unlocked(id, repo),
        };
        // Read on a clone so the original stream stays open for the whole
        // critical section — closing it is what signals RELEASE/auto-release.
        let reader_half = match stream.try_clone() {
            Ok(s) => s,
            Err(_) => return Self::unlocked(id, repo),
        };
        let mut reader = BufReader::new(reader_half);

        // Cross-process reentrancy. `HELD` above only covers this thread; a `git`
        // CHILD of a `git` parent that holds this lane is a different process and
        // would enqueue behind a parent that is blocked waiting on the child —
        // deadlock. Ask the daemon who holds the lane and, if it is one of our own
        // ancestors, hand back a no-op guard: the ancestor's hold already excludes
        // every other writer for as long as we run inside it.
        if matches!(lane_owner(&mut stream, &mut reader, &repo), LaneOwner::Ancestor) {
            return Self::unlocked(id, repo);
        }

        if stream
            .write_all(format!("ACQUIRE {id} {}\n", repo.display()).as_bytes())
            .is_err()
            || stream.flush().is_err()
        {
            return Self::unlocked(id, repo);
        }

        // Block until the daemon answers `GRANTED` (our turn at the FIFO head).
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(n) if n > 0 && line.trim() == "GRANTED" => Self {
                stream: Some(stream),
                id,
                held_key: Some(repo),
                _not_send: std::marker::PhantomData,
            },
            _ => Self::unlocked(id, repo),
        }
    }

    /// Non-blocking acquire. Unlike [`acquire`], never waits: if the repo's lane is
    /// already held, returns [`TryLock::Busy`] at once so the caller can queue its
    /// work as a job instead of blocking. `TryLock::Held` is the lock (proceed
    /// inline); `TryLock::NoDaemon` means no daemon is reachable (proceed
    /// unserialized, exactly as `acquire` does in that case).
    pub fn try_acquire(git_dir: &Path) -> TryLock {
        let id = format!("{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed));
        let sock = crate::superset::zdaemon::socket_path();
        let repo = lane_key(git_dir);

        // Reentrant: this thread already holds it — hand back a no-op held guard.
        if HELD.with(|h| h.borrow().contains(&repo)) {
            return TryLock::Held(Self { stream: None, id, held_key: None, _not_send: std::marker::PhantomData });
        }

        let mut stream = match UnixStream::connect(&sock) {
            Ok(s) => s,
            Err(_) => return TryLock::NoDaemon,
        };
        let reader_half = match stream.try_clone() {
            Ok(s) => s,
            Err(_) => return TryLock::NoDaemon,
        };
        let mut reader = BufReader::new(reader_half);

        // Same cross-process reentrancy as `acquire`, and the more important half:
        // without it a `git` child of a lane-holding `git` parent reads `BUSY`,
        // QUEUES ITSELF AS A JOB and exits 0, so the parent sees success on work
        // that never ran. An ancestor holding the lane is not contention.
        let owner = lane_owner(&mut stream, &mut reader, &repo);
        if matches!(owner, LaneOwner::Ancestor) {
            // Register the lane the way a granted acquire does, so the nested
            // `acquire` the porcelain makes a moment later takes the thread-local
            // shortcut instead of probing the daemon again.
            HELD.with(|h| {
                h.borrow_mut().insert(repo.clone());
            });
            return TryLock::Held(Self::unlocked(id, repo));
        }
        let owner_resolved = !matches!(owner, LaneOwner::Unknown);

        if stream.write_all(format!("TRYACQUIRE {id} {}\n", repo.display()).as_bytes()).is_err()
            || stream.flush().is_err()
        {
            return TryLock::NoDaemon;
        }
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(n) if n > 0 && line.trim() == "GRANTED" => {
                HELD.with(|h| {
                    h.borrow_mut().insert(repo.clone());
                });
                TryLock::Held(Self {
                    stream: Some(stream),
                    id,
                    held_key: Some(repo),
                    _not_send: std::marker::PhantomData,
                })
            }
            Ok(n) if n > 0 && line.trim() == "BUSY" => TryLock::Busy { owner_resolved },
            _ => TryLock::NoDaemon,
        }
    }

    /// No-op guard for the no-daemon / handshake-failed path. We already inserted
    /// `repo` into `HELD`, so this guard owns the key and clears it on drop.
    fn unlocked(id: String, repo: PathBuf) -> Self {
        Self { stream: None, id, held_key: Some(repo), _not_send: std::marker::PhantomData }
    }

    /// Whether this guard is backed by a live daemon (vs. the no-op fallback).
    pub fn is_held(&self) -> bool {
        self.stream.is_some()
    }
}

/// The daemon lane key for `git_dir`.
///
/// Single machine-wide daemon; the repo is identified by its git-dir, so the key
/// has to be the SAME string for every caller that means the same repository —
/// two spellings are two lanes, which is two concurrent writers.
///
/// `canonicalize` alone is not enough because it fails on a path that does not
/// exist YET, and `clone` takes the lane on `<dst>/.git` before creating it. On
/// macOS that produced a real split: the parent keyed the raw
/// `/tmp/x/.git` while every later caller keyed the resolved
/// `/private/tmp/x/.git`, so the two never contended — which is why
/// `clone --recurse-submodules` swallowed its `submodule update` child on some
/// paths and not others, and read as "intermittent".
///
/// So: canonicalize the deepest ancestor that DOES exist and re-attach the
/// missing tail. A path canonicalized before and after creation then agrees.
fn lane_key(git_dir: &Path) -> PathBuf {
    let abs = if git_dir.is_absolute() {
        git_dir.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|c| c.join(git_dir))
            .unwrap_or_else(|_| git_dir.to_path_buf())
    };
    if let Ok(real) = abs.canonicalize() {
        return real;
    }
    let mut missing_tail = Vec::new();
    let mut cursor = abs.as_path();
    while let (Some(parent), Some(name)) = (cursor.parent(), cursor.file_name()) {
        missing_tail.push(name.to_os_string());
        if let Ok(real_parent) = parent.canonicalize() {
            let mut key = real_parent;
            key.extend(missing_tail.iter().rev());
            return key;
        }
        cursor = parent;
    }
    abs
}

/// Who owns a repo's lane, relative to the asking process.
enum LaneOwner {
    /// One of this process's own ancestors — we are running INSIDE its critical
    /// section, so the lane is effectively ours and must not be waited on.
    Ancestor,
    /// Free, or held by someone unrelated. Contend for it normally.
    Other,
    /// The daemon would not say. Notably a coordinator predating the `HOLDER`
    /// query: we cannot rule out that the holder is our own ancestor, so neither
    /// waiting nor queueing is provably safe.
    Unknown,
}

/// Ask the daemon who holds `repo`'s lane.
///
/// Sent on the connection the caller is about to `ACQUIRE`/`TRYACQUIRE` on, so
/// the probe costs one extra write+read on an open socket rather than a second
/// connect, and only ever on a path that was already going to talk to the daemon.
///
/// An older daemon answers `ERR unknown verb "HOLDER"` and leaves the connection
/// open, so the caller's real request still goes through on the same stream —
/// the probe is safe to send at a coordinator of any vintage.
fn lane_owner(stream: &mut UnixStream, reader: &mut BufReader<UnixStream>, repo: &Path) -> LaneOwner {
    if stream.write_all(format!("HOLDER {}\n", repo.display()).as_bytes()).is_err()
        || stream.flush().is_err()
    {
        return LaneOwner::Unknown;
    }
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return LaneOwner::Unknown;
    }
    let Some(holder) = line.trim().strip_prefix("holder=") else {
        return LaneOwner::Unknown; // `ERR …` from a daemon that predates this verb
    };
    if holder == "none" {
        return LaneOwner::Other; // nobody holds it; whoever takes it next is not us
    }
    // The client id is `<pid>-<seq>`; the pid is all we need.
    match holder.split('-').next().and_then(|p| p.parse::<i64>().ok()) {
        Some(pid) if crate::superset::zppid::is_ancestor(pid) => LaneOwner::Ancestor,
        Some(_) => LaneOwner::Other,
        None => LaneOwner::Unknown,
    }
}

/// How long to wait for a FOREIGN `index.lock` to clear before giving up, in
/// milliseconds. Overridable with `ZVCS_INDEX_LOCK_WAIT_MS` (`0` disables the
/// wait entirely). Two seconds covers an IDE's or stock git's index write
/// without making a genuinely stuck lock feel like a hang.
const FOREIGN_LOCK_WAIT_MS: u64 = 2_000;

/// Wait for `<git_dir>/index.lock` to disappear, returning `true` once the path
/// is clear and `false` if it is still there when the budget runs out.
///
/// The daemon's FIFO only serializes zvcs writers. Anything else touching the
/// repo — stock git, an IDE, a hook shelling out — takes git's `O_EXCL`
/// lockfile, which the lane cannot see, and the ported gitoxide index writer
/// acquires that file with `Fail::Immediately` (one attempt, no wait). Polling
/// the path here turns the common case (a foreign writer holding it for
/// milliseconds) into a short wait instead of a hard failure; when the wait is
/// exhausted the caller queues the command as a job rather than erroring.
///
/// Polling, not inotify/kqueue: the wait is short, the file is on local disk,
/// and a portable poll keeps this identical on macOS and Linux.
pub fn wait_for_foreign_index_lock(git_dir: &Path) -> bool {
    let budget = std::env::var("ZVCS_INDEX_LOCK_WAIT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(FOREIGN_LOCK_WAIT_MS);
    let lock = git_dir.join("index.lock");
    if budget == 0 || !lock.exists() {
        return !lock.exists();
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(budget);
    // Back off 5ms → 100ms: a lock held for one index write clears on an early
    // cheap poll, while a long hold stops costing wakeups.
    let mut nap = std::time::Duration::from_millis(5);
    while std::time::Instant::now() < deadline {
        std::thread::sleep(nap);
        if !lock.exists() {
            return true;
        }
        nap = (nap * 2).min(std::time::Duration::from_millis(100));
    }
    !lock.exists()
}

/// Whether `err`'s chain contains a gitoxide lockfile-acquisition failure — the
/// `.git/index.lock` (or ref/config lock) a foreign writer is holding.
///
/// Matched by TYPE, not message: `gix_index::file::write::Error::AcquireLock`
/// wraps `gix::lock::acquire::Error` via `#[from]`, so the concrete error is
/// reachable through `anyhow`'s source chain and stays matched if the wording
/// changes.
pub fn is_lock_contention(err: &anyhow::Error) -> bool {
    err.chain().any(|e| e.is::<gix::lock::acquire::Error>())
}

/// Whether `err`'s chain is a REF RACE: the compare-and-swap on a reference was
/// rejected because a concurrent writer moved it between our read and our write
/// (`The reference "refs/heads/main" should have content <a>, actual content was
/// <b>`).
///
/// This is contention with a different shape than [`is_lock_contention`]. No
/// lockfile is involved — both writers took `.git/index` and the ref lock
/// cleanly, one simply lost the race on the ref's expected value. Under a
/// N-agent fanout with no daemon (the unserialized fallback), this is the
/// dominant loss mode: a 32-way `commit` fanout produced 12 of these, each one a
/// hard exit-1 that dropped the commit, versus 2 lockfile errors.
///
/// Re-running the command once the winner has landed is exactly what resolves
/// it, so the caller queues it like a lock conflict instead of failing.
///
/// Matched by TYPE and VARIANT, at every wrapper level the porcelain can hand
/// us. The wrappers are `#[error(transparent)]`, and transparent forwards
/// `source()` to the INNER error's source — so the inner
/// `prepare::Error` is NOT a link in `anyhow`'s chain, it is the payload of the
/// link that is there. Downcasting to it alone silently matches nothing (which
/// is how this shipped un-caught). Hence one arm per reachable carrier:
/// `commit`/`commit_as` yield `gix::commit::Error`, `--amend` and the branch/tag
/// verbs yield `gix::reference::edit::Error`, and a bare transaction yields the
/// `prepare::Error` itself.
///
/// Only `ReferenceOutOfDate` counts — the other variants (a malformed name, an
/// IO failure) are real errors that a re-run would hit again.
pub fn is_ref_race(err: &anyhow::Error) -> bool {
    /// The one variant that means "someone else won the race".
    fn lost_cas(e: &gix::refs::file::transaction::prepare::Error) -> bool {
        matches!(e, gix::refs::file::transaction::prepare::Error::ReferenceOutOfDate { .. })
    }

    err.chain().any(|e| {
        if let Some(p) = e.downcast_ref::<gix::refs::file::transaction::prepare::Error>() {
            return lost_cas(p);
        }
        if let Some(gix::reference::edit::Error::FileTransactionPrepare(p)) =
            e.downcast_ref::<gix::reference::edit::Error>()
        {
            return lost_cas(p);
        }
        if let Some(gix::commit::Error::ReferenceEdit(
            gix::reference::edit::Error::FileTransactionPrepare(p),
        )) = e.downcast_ref::<gix::commit::Error>()
        {
            return lost_cas(p);
        }
        false
    })
}

impl Drop for RepoLock {
    fn drop(&mut self) {
        if let Some(stream) = self.stream.as_mut() {
            let _ = stream.write_all(format!("RELEASE {}\n", self.id).as_bytes());
            let _ = stream.flush();
            // Closing the socket (on drop, right after this) also triggers the
            // daemon's EOF auto-release, so the next waiter is promoted either way.
        }
        // Clear the thread-local reentrancy key (only the guard that registered it
        // carries `held_key`; reentrant no-op guards carry `None` and skip this).
        if let Some(key) = self.held_key.take() {
            HELD.with(|h| {
                h.borrow_mut().remove(&key);
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use gix::refs::file::transaction::prepare::Error as PrepareError;

    /// A `ReferenceOutOfDate` as the ref store raises it when a concurrent writer
    /// moved `refs/heads/main` between our read and our compare-and-swap, wrapped
    /// the way a porcelain command surfaces it (`anyhow` with context on top).
    fn lost_ref_race() -> anyhow::Error {
        let err = PrepareError::ReferenceOutOfDate {
            full_name: "refs/heads/main".into(),
            expected: gix::refs::Target::Object(gix::ObjectId::empty_blob(gix::hash::Kind::Sha1)),
            actual: gix::refs::Target::Object(gix::ObjectId::empty_tree(gix::hash::Kind::Sha1)),
        };
        anyhow::Error::new(err).context("commit")
    }

    /// The two contention shapes must not be conflated: a ref race takes no
    /// lockfile, so the lockfile predicate has to stay `false` for it. Before the
    /// ref-race arm existed, that `false` is exactly what dropped the commit
    /// instead of queueing it.
    #[test]
    fn a_ref_race_is_contention_but_not_lock_contention() {
        let err = lost_ref_race();
        assert!(super::is_ref_race(&err), "the CAS rejection is a ref race");
        assert!(
            !super::is_lock_contention(&err),
            "no lockfile was involved — this must not read as lock contention"
        );
    }

    /// The same rejection as the porcelain actually produces it: through `gix`'s
    /// `edit_reference`, so the wrapper chain (`reference::edit::Error` →
    /// `prepare::Error`) is the real one and not a hand-built stand-in. A
    /// constructed variant alone would still pass if the wrapper stopped
    /// forwarding `source()`.
    #[test]
    fn the_real_edit_reference_rejection_is_recognized() {
        let dir = std::env::temp_dir().join(format!("zvcs-refrace-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let repo = gix::init(&dir).expect("init repo");

        // A ref whose value we then claim to know — the CAS is rejected because the
        // value we pass as `expected` is not what is on disk.
        let stale = gix::ObjectId::empty_blob(gix::hash::Kind::Sha1);
        let actual = repo.write_blob(b"on disk").expect("write blob").detach();
        let name: gix::refs::FullName = "refs/heads/race".try_into().expect("ref name");
        repo.edit_reference(gix::refs::transaction::RefEdit {
            change: gix::refs::transaction::Change::Update {
                log: gix::refs::transaction::LogChange::default(),
                expected: gix::refs::transaction::PreviousValue::Any,
                new: gix::refs::Target::Object(actual),
            },
            name: name.clone(),
            deref: false,
        })
        .expect("create ref");

        // Claim a value the ref does not have: the store rejects the swap.
        let lose_the_race = || {
            repo.edit_reference(gix::refs::transaction::RefEdit {
                change: gix::refs::transaction::Change::Update {
                    log: gix::refs::transaction::LogChange::default(),
                    expected: gix::refs::transaction::PreviousValue::MustExistAndMatch(
                        gix::refs::Target::Object(stale),
                    ),
                    new: gix::refs::Target::Object(actual),
                },
                name: name.clone(),
                deref: false,
            })
            .expect_err("the stale expectation must be rejected")
        };

        // `--amend` and the branch/tag verbs surface exactly this.
        let as_edit = anyhow::Error::new(lose_the_race());
        assert!(super::is_ref_race(&as_edit), "unrecognized as edit error: {as_edit:#}");

        // `git commit` goes through `Repository::commit`, one wrapper further out.
        let as_commit = anyhow::Error::new(gix::commit::Error::ReferenceEdit(lose_the_race()));
        assert!(super::is_ref_race(&as_commit), "unrecognized as commit error: {as_commit:#}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Only the CAS rejection queues. A malformed ref name is a real error a
    /// re-run would hit identically, so classifying it as contention would spin
    /// the queue on a command that can never succeed.
    #[test]
    fn other_prepare_failures_are_not_races() {
        let err = anyhow::Error::new(PrepareError::MustExist {
            full_name: "refs/heads/main".into(),
            expected: gix::refs::Target::Object(gix::ObjectId::empty_blob(gix::hash::Kind::Sha1)),
        });
        assert!(!super::is_ref_race(&err));
    }
}
