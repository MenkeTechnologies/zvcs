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

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// Per-process monotonic counter making every lock id unique even when one
/// process acquires from multiple threads (the daemon keys holder/release on the
/// id, so two live acquisitions must never share one).
static SEQ: AtomicU64 = AtomicU64::new(0);

/// Held index lock. While alive, this process is the sole writer the daemon has
/// granted for the repo. Release is automatic on drop.
#[must_use = "the lock releases when this guard is dropped; bind it for the critical section"]
pub struct RepoLock {
    /// `Some` when held via a live daemon; `None` for the no-daemon no-op guard.
    stream: Option<UnixStream>,
    id: String,
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
        let sock = git_dir.join("zvcs.sock");

        let mut stream = match UnixStream::connect(&sock) {
            Ok(s) => s,
            Err(_) => return Self::unlocked(id),
        };
        if stream.write_all(format!("ACQUIRE {id}\n").as_bytes()).is_err() || stream.flush().is_err()
        {
            return Self::unlocked(id);
        }

        // Block until the daemon answers `GRANTED` (our turn at the FIFO head).
        // Read on a clone so the original stream stays open for the whole
        // critical section — closing it is what signals RELEASE/auto-release.
        let reader_half = match stream.try_clone() {
            Ok(s) => s,
            Err(_) => return Self::unlocked(id),
        };
        let mut reader = BufReader::new(reader_half);
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(n) if n > 0 && line.trim() == "GRANTED" => Self {
                stream: Some(stream),
                id,
            },
            _ => Self::unlocked(id),
        }
    }

    fn unlocked(id: String) -> Self {
        Self { stream: None, id }
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
    }
}
