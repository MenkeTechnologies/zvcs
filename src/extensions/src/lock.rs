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
        // Single machine-wide daemon; the repo is identified by its git-dir so
        // the daemon serializes each repo on its own lane. Canonicalize so every
        // caller for the same repo produces the same lane key; on failure still
        // produce an ABSOLUTE key — a relative fallback could put a concurrent
        // caller of the same repo on a different lane (two lanes = two writers).
        let sock = crate::superset::zdaemon::socket_path();
        let repo = git_dir.canonicalize().unwrap_or_else(|_| {
            if git_dir.is_absolute() {
                git_dir.to_path_buf()
            } else {
                std::env::current_dir()
                    .map(|c| c.join(git_dir))
                    .unwrap_or_else(|_| git_dir.to_path_buf())
            }
        });

        // Reentrancy: if this thread already holds this repo, hand back a no-op
        // guard. A nested acquire that went to the daemon would queue behind the
        // outer hold and block forever — the outer guard can't drop while this
        // same thread is blocked in `read_line` (self-deadlock). A *different*
        // thread/process still goes to the daemon and serializes normally.
        if HELD.with(|h| h.borrow().contains(&repo)) {
            return Self { stream: None, id, held_key: None };
        }
        HELD.with(|h| {
            h.borrow_mut().insert(repo.clone());
        });

        let mut stream = match UnixStream::connect(&sock) {
            Ok(s) => s,
            Err(_) => return Self::unlocked(id, repo),
        };
        if stream
            .write_all(format!("ACQUIRE {id} {}\n", repo.display()).as_bytes())
            .is_err()
            || stream.flush().is_err()
        {
            return Self::unlocked(id, repo);
        }

        // Block until the daemon answers `GRANTED` (our turn at the FIFO head).
        // Read on a clone so the original stream stays open for the whole
        // critical section — closing it is what signals RELEASE/auto-release.
        let reader_half = match stream.try_clone() {
            Ok(s) => s,
            Err(_) => return Self::unlocked(id, repo),
        };
        let mut reader = BufReader::new(reader_half);
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(n) if n > 0 && line.trim() == "GRANTED" => Self {
                stream: Some(stream),
                id,
                held_key: Some(repo),
            },
            _ => Self::unlocked(id, repo),
        }
    }

    /// No-op guard for the no-daemon / handshake-failed path. We already inserted
    /// `repo` into `HELD`, so this guard owns the key and clears it on drop.
    fn unlocked(id: String, repo: PathBuf) -> Self {
        Self { stream: None, id, held_key: Some(repo) }
    }

    /// Whether this guard is backed by a live daemon (vs. the no-op fallback).
    pub fn is_held(&self) -> bool {
        self.stream.is_some()
    }
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
