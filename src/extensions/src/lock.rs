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
//! If no daemon is reachable the lock falls back to a **lane file** —
//! `<git_dir>/zvcs-lane.lock`, held with `flock(LOCK_EX)` for the whole command.
//! It has to hold *something*: the port's only index lock is the one
//! `gix_index::File::write()` takes at WRITE time
//! (`gix-index/src/file/write.rs:85`), which is far too late. Git takes
//! `.git/index.lock` BEFORE it reads the index — in `builtin/add.c`,
//! `repo_hold_locked_index(repo, &lock_file, LOCK_DIE_ON_ERROR)` precedes
//! `repo_read_index_preload()` — so the whole read-modify-write is inside the
//! lock. Guarding only the write leaves two writers free to read the same base
//! index and write back their own copies; the loser's entry is gone and both
//! processes exit 0. Measured: eight concurrent `git add`s on distinct paths, no
//! daemon, lost a write in nine of ten trials.
//!
//! The lane file is deliberately NOT `index.lock`: gitoxide acquires that one
//! with `Fail::Immediately`, so holding it ourselves makes our own writer fail
//! ("could not be obtained immediately after 1 attempt(s)"). A separate
//! zvcs-owned file gives zvcs-vs-zvcs exclusion without touching the writer, and
//! foreign writers are covered separately by [`wait_for_foreign_index_lock`].
//!
//! Exclusion is the kernel's `flock`, not the file's existence, so a killed
//! holder wedges nothing: the lock dies with the process even on `SIGKILL`, and
//! the leftover file is just a stale pid label. Ensuring a daemon is running is
//! the autonomous layer's job, not the writer's.

use std::cell::RefCell;
use std::collections::HashSet;
use std::io::{BufRead, BufReader, Seek, Write};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

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
    /// `Some` when held via a live daemon; `None` for the no-daemon fallback.
    stream: Option<UnixStream>,
    /// `Some` when this guard holds the no-daemon lane file. Mutually exclusive
    /// with `stream`: a daemon-granted holder already excludes every other zvcs
    /// writer, and a reentrant guard must hold neither.
    lane: Option<LaneFile>,
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
    /// Neither a daemon nor a lane file is available (a git dir that does not
    /// exist yet, or a mount without `flock`) — proceed unserialized. Contention
    /// with a live daemon is `Busy`; contention without one never reaches the
    /// caller, it is refused inside the lock.
    NoDaemon,
}

impl RepoLock {
    /// Acquire the repo-wide index lock via the daemon at `<git_dir>/zvcs.sock`.
    ///
    /// Blocks in the daemon's fair FIFO until granted. If the daemon is
    /// unreachable or the handshake does not complete, falls back to the lane
    /// FILE (`<git_dir>/zvcs-lane.lock`, `flock`-held for the guard's life) so
    /// concurrent zvcs writers still serialize. Returns only when the caller may
    /// safely write: a lane a live stranger will not release within the budget
    /// exits the process non-zero instead of handing back a guard that excludes
    /// nobody.
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
            return Self { stream: None, lane: None, id, held_key: None, _not_send: std::marker::PhantomData };
        }
        HELD.with(|h| {
            h.borrow_mut().insert(repo.clone());
        });

        let mut stream = match UnixStream::connect(&sock) {
            Ok(s) => s,
            Err(_) => return Self::fallback(id, repo),
        };
        // Read on a clone so the original stream stays open for the whole
        // critical section — closing it is what signals RELEASE/auto-release.
        let reader_half = match stream.try_clone() {
            Ok(s) => s,
            Err(_) => return Self::fallback(id, repo),
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
            return Self::fallback(id, repo);
        }

        // Block until the daemon answers `GRANTED` (our turn at the FIFO head).
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(n) if n > 0 && line.trim() == "GRANTED" => Self {
                stream: Some(stream),
                lane: None,
                id,
                held_key: Some(repo),
                _not_send: std::marker::PhantomData,
            },
            _ => Self::fallback(id, repo),
        }
    }

    /// Non-blocking acquire — while there is a daemon. If the repo's lane is held,
    /// returns [`TryLock::Busy`] at once so the caller can queue its work as a job
    /// instead of blocking. `TryLock::Held` is the lock (proceed inline);
    /// `TryLock::NoDaemon` means neither form of exclusion exists here at all.
    ///
    /// With no daemon there is no queue to defer to, so the fallback waits on the
    /// lane file within the budget rather than reporting `Busy` — deferring to a
    /// queue nobody is running would be the same silent success as not locking.
    pub fn try_acquire(git_dir: &Path) -> TryLock {
        let id = format!("{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed));
        let sock = crate::superset::zdaemon::socket_path();
        let repo = lane_key(git_dir);

        // Reentrant: this thread already holds it — hand back a no-op held guard.
        if HELD.with(|h| h.borrow().contains(&repo)) {
            return TryLock::Held(Self { stream: None, lane: None, id, held_key: None, _not_send: std::marker::PhantomData });
        }

        let mut stream = match UnixStream::connect(&sock) {
            Ok(s) => s,
            Err(_) => return Self::fallback_try(id, repo),
        };
        let reader_half = match stream.try_clone() {
            Ok(s) => s,
            Err(_) => return Self::fallback_try(id, repo),
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
            return Self::fallback_try(id, repo);
        }
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(n) if n > 0 && line.trim() == "GRANTED" => {
                HELD.with(|h| {
                    h.borrow_mut().insert(repo.clone());
                });
                TryLock::Held(Self {
                    stream: Some(stream),
                    lane: None,
                    id,
                    held_key: Some(repo),
                    _not_send: std::marker::PhantomData,
                })
            }
            Ok(n) if n > 0 && line.trim() == "BUSY" => TryLock::Busy { owner_resolved },
            _ => Self::fallback_try(id, repo),
        }
    }

    /// No-op guard: no daemon lock, no lane file. For the two cases where holding
    /// anything would be wrong — a reentrant acquire, and an acquire whose lane is
    /// already held by one of our own ancestors. We already inserted `repo` into
    /// `HELD`, so this guard owns the key and clears it on drop.
    fn unlocked(id: String, repo: PathBuf) -> Self {
        Self { stream: None, lane: None, id, held_key: Some(repo), _not_send: std::marker::PhantomData }
    }

    /// Guard for the no-daemon / handshake-failed path: take the lane FILE, so
    /// concurrent zvcs writers still serialize.
    ///
    /// Exits non-zero rather than returning if the lane cannot be had (see
    /// [`deny_and_exit`]) — the one thing this must never do is hand back a guard
    /// that excludes nobody and let the caller report success on a write another
    /// process is about to erase.
    fn fallback(id: String, repo: PathBuf) -> Self {
        let lane = match take_lane(&repo) {
            LaneOutcome::Taken(lane) => Some(lane),
            // An ancestor holds it: we run INSIDE its critical section, and
            // waiting would be waiting on a process that is waiting on us.
            // Nothing else can be running, so holding nothing is correct.
            LaneOutcome::Reentrant => None,
            LaneOutcome::Unavailable => None,
            LaneOutcome::Denied { holder } => deny_and_exit(&repo, holder),
        };
        Self { stream: None, lane, id, held_key: Some(repo), _not_send: std::marker::PhantomData }
    }

    /// [`try_acquire`](Self::try_acquire)'s no-daemon answer: the lane file is the
    /// only exclusion left, so take it and report `Held`. `NoDaemon` now means the
    /// strictly weaker thing — not even a lane file is available — which is the
    /// only case where dispatch still runs the command unserialized.
    fn fallback_try(id: String, repo: PathBuf) -> TryLock {
        let lane = match take_lane(&repo) {
            LaneOutcome::Taken(lane) => Some(lane),
            LaneOutcome::Reentrant => None,
            LaneOutcome::Unavailable => return TryLock::NoDaemon,
            LaneOutcome::Denied { holder } => deny_and_exit(&repo, holder),
        };
        // Register the lane the way a granted acquire does, so the porcelain's
        // own `acquire` a moment later takes the thread-local shortcut instead of
        // deadlocking on the lane file this very guard holds.
        HELD.with(|h| {
            h.borrow_mut().insert(repo.clone());
        });
        TryLock::Held(Self {
            stream: None,
            lane,
            id,
            held_key: Some(repo),
            _not_send: std::marker::PhantomData,
        })
    }

    /// Whether this guard is backed by a live daemon (vs. the lane-file fallback
    /// or a reentrant no-op). Callers use it to assert the coordinator was in
    /// play, not to ask whether anything is excluded — see [`Self::excludes`].
    pub fn is_held(&self) -> bool {
        self.stream.is_some()
    }

    /// Whether this guard actually excludes other zvcs writers — by daemon grant
    /// or by lane file. A reentrant guard answers `false` because its exclusion
    /// belongs to the outer guard, not to it.
    pub fn excludes(&self) -> bool {
        self.stream.is_some() || self.lane.is_some()
    }
}

// ---------------------------------------------------------------------------
// The no-daemon lane file
// ---------------------------------------------------------------------------

/// The lane file inside a git dir. Not `index.lock`: that name belongs to the
/// index writer, which takes it with `Fail::Immediately` and would fail against
/// us. This one is zvcs's own.
const LANE_FILE_NAME: &str = "zvcs-lane.lock";

/// A held lane. The `flock` lives on the open descriptor, so dropping this —
/// normal return, `?`, panic, `SIGKILL`, `kill -9` — releases it; that is the
/// whole reason exclusion is the kernel lock and not the file's existence.
///
/// The file is never unlinked. Unlinking races: a waiter blocked on `flock` of
/// the now-deleted inode would be granted a lock on a file nobody else can find,
/// while the next writer creates a fresh one and is granted it too. A leftover
/// zero-to-few-byte file naming a dead pid costs nothing and blocks nobody.
struct LaneFile {
    _file: std::fs::File,
}

impl LaneFile {
    /// Publish which process holds the lane, so a waiter can tell an ANCESTOR's
    /// hold (we are inside its critical section — proceed) from a stranger's
    /// (contention — wait).
    ///
    /// Truncate BEFORE writing: between taking the lock and naming ourselves the
    /// file must not still name the previous holder, or a waiter could read a pid
    /// that happens to be one of its own ancestors and skip a lock somebody else
    /// is holding. An empty read means "held, holder not yet published", and the
    /// waiter simply polls again a millisecond later.
    fn publish(mut file: std::fs::File) -> Self {
        let _ = file.set_len(0);
        let _ = file.seek(std::io::SeekFrom::Start(0));
        let _ = file.write_all(format!("{}\n", std::process::id()).as_bytes());
        let _ = file.flush();
        Self { _file: file }
    }
}

/// What [`take_lane`] found.
enum LaneOutcome {
    /// The lane is ours for as long as the guard lives.
    Taken(LaneFile),
    /// One of our own ancestors holds it. We are running inside its critical
    /// section: waiting would deadlock, and no other writer can be running.
    Reentrant,
    /// No lane file can exist here — the key's directory does not exist yet
    /// (`clone` takes the lane on `<dst>/.git` before creating it), or the
    /// filesystem does not implement `flock`. Nothing to lose in the first case;
    /// in the second the caller is back to the unserialized behavior this
    /// fallback replaced, which is the best available on such a mount.
    Unavailable,
    /// A live stranger has held it past the budget. The caller must NOT proceed.
    Denied { holder: Option<i64> },
}

/// Where the lane file for `key` lives.
///
/// `key` is normally a git dir, and the lane file goes inside it. `config.rs`
/// also locks bare config FILES (`~/.gitconfig`, a `--file` target), so those get
/// a sibling `<name>.zvcs-lane.lock` instead of a path inside a non-directory.
/// A key that does not exist at all has no lane: see [`LaneOutcome::Unavailable`].
/// Where a repository's lane file lives — **outside** the repository.
///
/// It used to sit at `<git_dir>/zvcs-lane.lock`, which was wrong for a reason
/// that has nothing to do with locking: the git directory's contents are part of
/// what this port is measured on. `revert_auto_merge_state` compares the whole
/// `.git` listing against stock git's after each verb, and every one of its
/// cases failed on the extra entry — correctly, because a repository that stock
/// git would not have written is exactly what that test exists to catch. The
/// file also survives the process that made it (unlinking races a waiter already
/// blocked on the inode), so it is not transient enough to excuse.
///
/// `flock` does not care where the inode is, only that every writer agrees on
/// it, so the lane moves beside the daemon's own socket and is keyed by a hash
/// of the canonical git directory. Two processes reaching one repository by
/// different paths still agree, because [`lane_key`] canonicalises before this
/// is called.
fn lane_lock_path(key: &Path) -> Option<PathBuf> {
    use std::hash::{Hash, Hasher};
    let dir = crate::superset::zdaemon::zvcs_home().join("lanes");
    std::fs::create_dir_all(&dir).ok()?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut hasher);
    Some(dir.join(format!("{:016x}.lock", hasher.finish())))
}

/// The pid the lane file names, or `None` if it names nothing readable yet.
fn lane_holder(path: &Path) -> Option<i64> {
    std::fs::read_to_string(path).ok()?.trim().parse::<i64>().ok()
}

/// Take `key`'s lane file, waiting for a holder within the budget.
///
/// The budget ([`ZVCS_INDEX_LOCK_WAIT_MS`](FOREIGN_LOCK_WAIT_MS)) is patience for
/// ONE holder, not for the whole queue: the deadline restarts whenever the pid in
/// the file changes, because a lane that keeps changing hands is making progress
/// and a fair wait behind eight writers must not be mistaken for a wedge. Only a
/// single holder that sits on the lane past the budget is denied — and that
/// holder is provably alive, because `flock` is released by the kernel the
/// instant its process dies.
fn take_lane(key: &Path) -> LaneOutcome {
    let Some(path) = lane_lock_path(key) else {
        return LaneOutcome::Unavailable;
    };
    let file = match std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path) {
        Ok(f) => f,
        Err(_) => return LaneOutcome::Unavailable,
    };

    let budget = Duration::from_millis(lock_wait_budget());
    let mut deadline = Instant::now() + budget;
    // 1ms → 20ms: the common contended wait is one short index write, which a
    // cheap early poll catches; a long hold stops costing wakeups.
    let mut nap = Duration::from_millis(1);
    let mut seen: Option<i64> = None;
    loop {
        // SAFETY: `file` owns the descriptor for the whole call and `flock` only
        // reads it. `LOCK_NB` means this never blocks in the kernel, so the wait
        // stays ours to bound.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return LaneOutcome::Taken(LaneFile::publish(file));
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::EINTR) => continue,
            // The lane is held by somebody.
            Some(libc::EWOULDBLOCK) => {}
            // `ENOLCK`, `EOPNOTSUPP` — a mount without `flock`. No exclusion is
            // available here at all; say so rather than pretending.
            _ => return LaneOutcome::Unavailable,
        }

        let holder = lane_holder(&path);
        if let Some(pid) = holder {
            if crate::superset::zppid::is_ancestor(pid) {
                return LaneOutcome::Reentrant;
            }
            if seen != Some(pid) {
                seen = Some(pid);
                deadline = Instant::now() + budget;
            }
        }
        if Instant::now() >= deadline {
            return LaneOutcome::Denied { holder };
        }
        std::thread::sleep(nap);
        nap = (nap * 2).min(Duration::from_millis(20));
    }
}

/// Refuse the command rather than run it unserialized.
///
/// This is the whole point of the fallback. Running anyway would read an index
/// another live writer is about to overwrite, report success, and lose the
/// change — the failure this lock exists to prevent. Git makes the same call:
/// `builtin/add.c` passes `LOCK_DIE_ON_ERROR`, and `lockfile.h` documents it as
/// "if a lock is already taken for the file, `die()` with an error message".
///
/// Exits rather than returning an error because [`RepoLock::acquire`] is
/// infallible by signature at ~50 call sites; giving all of them a `Result` to
/// ignore would reintroduce exactly the silent-success shape this removes.
fn deny_and_exit(key: &Path, holder: Option<i64>) -> ! {
    let who = match holder {
        Some(pid) => format!("pid {pid}"),
        None => "another process".to_string(),
    };
    let lane = lane_lock_path(key).unwrap_or_else(|| key.join(LANE_FILE_NAME));
    eprintln!(
        "zvcs: {} is held by {who} and did not release within {}ms; refusing to run \
         unserialized. Two writers that both read this index would each write back their own \
         copy and the loser's change would vanish with a zero exit status. Retry, raise \
         ZVCS_INDEX_LOCK_WAIT_MS, or start the coordinator: git zdaemon start",
        lane.display(),
        lock_wait_budget(),
    );
    std::process::exit(1)
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

/// How long to wait for a single lock holder before giving up, in milliseconds.
/// Overridable with `ZVCS_INDEX_LOCK_WAIT_MS` (`0` disables the wait entirely).
/// Two seconds covers an IDE's or stock git's index write without making a
/// genuinely stuck lock feel like a hang.
///
/// One budget, two holders: a FOREIGN `index.lock`
/// ([`wait_for_foreign_index_lock`]) and a zvcs peer on the lane file
/// ([`take_lane`]). Both are "somebody else is mid-index-write"; a user who
/// widens one means to widen the other.
const FOREIGN_LOCK_WAIT_MS: u64 = 2_000;

/// The configured lock-wait budget in milliseconds.
fn lock_wait_budget() -> u64 {
    std::env::var("ZVCS_INDEX_LOCK_WAIT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(FOREIGN_LOCK_WAIT_MS)
}

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
    let budget = lock_wait_budget();
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
