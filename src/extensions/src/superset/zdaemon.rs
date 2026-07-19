//! `git zdaemon <start|stop|status>` — per-repo fair mutual-exclusion coordinator.
//!
//! This is the linchpin of zvcs's concurrency story. Git serializes index writes
//! with an `index.lock` file created via `O_EXCL`; a contended writer does not
//! wait, it *fails* (`fatal: Unable to create '.git/index.lock': File exists`).
//! Under many concurrent agents that turns a queue into a thundering herd of
//! retries with no fairness guarantee.
//!
//! `zdaemon` replaces that flock with a FIFO userspace barrier. A single worker
//! thread owns the abstract critical section and drains an mpsc channel of
//! requests *in arrival order*. That arrival order IS the fairness guarantee:
//! first-come-first-served, no starvation, no lost wakeups. A contended
//! `ACQUIRE` blocks in the queue and is answered `GRANTED` only when its turn
//! comes, instead of failing.
//!
//! # Wire protocol (line-based, one request per line, over the unix socket)
//!
//! Socket path: `<git-dir>/zvcs.sock`.
//!
//! Client -> daemon:
//!   * `ACQUIRE <client-id>` — enqueue a lock request. The daemon replies
//!     `GRANTED` on this same stream when (and only when) the caller reaches the
//!     head of the FIFO and holds the lock. The client keeps the connection open
//!     while it holds the lock.
//!   * `RELEASE <client-id>` — the current holder releases; the daemon grants the
//!     next queued waiter (if any).
//!   * `STATUS` — daemon replies one line `holder=<id|none> queue=<depth>` and the
//!     connection is closed.
//!   * `STOP` — daemon replies `STOPPING`, removes the socket, and exits.
//!
//! Daemon -> client:
//!   * `GRANTED` — the lock is now yours (in response to `ACQUIRE`).
//!   * `holder=<id|none> queue=<n>` — status snapshot.
//!   * `STOPPING` — acknowledging shutdown.
//!   * `ERR <reason>` — malformed request.
//!
//! # Crash-holder auto-release
//!
//! The lock holder keeps its `ACQUIRE` connection open for the duration of the
//! critical section. The per-connection reader thread blocks on `read_line`. If
//! the holder process crashes (or simply drops the socket), the socket hits EOF,
//! `read_line` returns `0`, and the connection thread synthesizes a `RELEASE` for
//! the id it acquired. The worker then grants the next waiter. A crashed holder
//! therefore cannot deadlock the repo — the lock is released automatically. The
//! same path also cancels a *queued* waiter that disconnects before its turn.

use anyhow::Result;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

/// Requests handed from per-connection reader threads to the single worker
/// thread that owns the critical section. Each `Acquire`/`Status`/`Stop`
/// carries a clone of the client's stream so the worker can reply on it at the
/// moment the request is serviced — possibly long after arrival, for a queued
/// `Acquire`.
enum Cmd {
    Acquire { id: String, stream: UnixStream },
    Release { id: String },
    Status { stream: UnixStream },
    Stop { stream: UnixStream },
}

/// Resolve `<git-dir>/zvcs.sock`. Falls back to `.git/zvcs.sock` if discovery
/// fails (e.g. run outside a repo during setup).
fn socket_path() -> PathBuf {
    match gix::discover(".") {
        Ok(repo) => repo.git_dir().join("zvcs.sock"),
        Err(_) => Path::new(".git").join("zvcs.sock"),
    }
}

/// Best-effort single-line reply. Never panics; a dead client just means the
/// write fails and is dropped — the worker must keep serving everyone else.
fn reply(mut stream: &UnixStream, msg: &str) {
    let _ = stream.write_all(msg.as_bytes());
    let _ = stream.write_all(b"\n");
    let _ = stream.flush();
}

pub fn zdaemon(args: &[String]) -> Result<ExitCode> {
    let action = args.first().map(String::as_str).unwrap_or("");
    match action {
        "start" => start(),
        "status" => status(),
        "stop" => stop(),
        "" => anyhow::bail!("usage: git zdaemon <start|stop|status>"),
        other => anyhow::bail!("unknown action {other:?} (want start|stop|status)"),
    }
}

/// Probe whether a live daemon owns the socket. Connects and sends `STATUS`;
/// a readable reply line means "running". A refused/failed connect means the
/// socket is stale or absent.
fn ping(path: &Path) -> bool {
    match UnixStream::connect(path) {
        Ok(mut stream) => {
            if stream.write_all(b"STATUS\n").is_err() || stream.flush().is_err() {
                return false;
            }
            let mut reader = BufReader::new(&stream);
            let mut line = String::new();
            matches!(reader.read_line(&mut line), Ok(n) if n > 0)
        }
        Err(_) => false,
    }
}

fn start() -> Result<ExitCode> {
    let path = socket_path();

    if path.exists() {
        if ping(&path) {
            anyhow::bail!("daemon already running");
        }
        // Stale socket from a crashed daemon: reclaim it.
        let _ = std::fs::remove_file(&path);
    }

    let listener = UnixListener::bind(&path)
        .map_err(|e| anyhow::anyhow!("cannot bind {}: {e}", path.display()))?;

    let (tx, rx): (Sender<Cmd>, Receiver<Cmd>) = mpsc::channel();

    // The one thread that owns the critical section. It never dies from a bad
    // request; only `STOP` ends it (via process exit after socket removal).
    let worker_path = path.clone();
    thread::spawn(move || worker_loop(rx, worker_path));

    // Accept loop: one reader thread per connection, each holding a Sender clone.
    for incoming in listener.incoming() {
        let stream = match incoming {
            Ok(s) => s,
            Err(_) => continue, // transient accept error: keep serving
        };
        let tx = tx.clone();
        thread::spawn(move || handle_conn(stream, tx));
    }

    Ok(ExitCode::SUCCESS)
}

/// The critical-section owner. Grants the lock to exactly one holder at a time
/// in strict FIFO order.
fn worker_loop(rx: Receiver<Cmd>, sock_path: PathBuf) {
    let mut holder: Option<String> = None;
    // Waiters that have not yet been granted, in arrival order. Each carries the
    // stream to answer `GRANTED` on when it reaches the head.
    let mut queue: VecDeque<(String, UnixStream)> = VecDeque::new();

    while let Ok(cmd) = rx.recv() {
        match cmd {
            Cmd::Acquire { id, stream } => {
                if holder.is_none() {
                    reply(&stream, "GRANTED");
                    holder = Some(id);
                } else {
                    queue.push_back((id, stream));
                }
            }
            Cmd::Release { id } => {
                if holder.as_deref() == Some(id.as_str()) {
                    // Real holder releasing: promote the next waiter, if any.
                    if let Some((next_id, next_stream)) = queue.pop_front() {
                        reply(&next_stream, "GRANTED");
                        holder = Some(next_id);
                    } else {
                        holder = None;
                    }
                } else {
                    // Not the holder: a queued waiter cancelled (e.g. crashed
                    // before its turn). Drop it from the queue; ordering of the
                    // rest is preserved.
                    queue.retain(|(qid, _)| qid != &id);
                }
            }
            Cmd::Status { stream } => {
                let h = holder.as_deref().unwrap_or("none");
                reply(&stream, &format!("holder={} queue={}", h, queue.len()));
            }
            Cmd::Stop { stream } => {
                reply(&stream, "STOPPING");
                let _ = std::fs::remove_file(&sock_path);
                std::process::exit(0);
            }
        }
    }
}

/// Per-connection reader. Parses line requests and forwards them to the worker.
/// On EOF/read error it auto-releases whatever lock this connection acquired so
/// a crashed holder can never wedge the repo.
fn handle_conn(stream: UnixStream, tx: Sender<Cmd>) {
    // The id this connection acquired the lock as, if any. Set on ACQUIRE,
    // cleared on explicit RELEASE. If still set when the loop ends (EOF/error),
    // we synthesize a RELEASE — the crash-holder auto-release path.
    let mut held_id: Option<String> = None;
    conn_loop(&stream, &tx, &mut held_id);
    if let Some(id) = held_id {
        let _ = tx.send(Cmd::Release { id });
    }
}

/// Drives the read loop for one connection. Returns when the socket closes, on a
/// read error, or after a terminal request (`STATUS`/`STOP`). Never panics.
fn conn_loop(stream: &UnixStream, tx: &Sender<Cmd>, held_id: &mut Option<String>) {
    let reader_stream = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut reader = BufReader::new(reader_stream);
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return,      // EOF: peer closed / crashed.
            Ok(_) => {}
            Err(_) => return,     // broken pipe / reset.
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut parts = trimmed.splitn(2, char::is_whitespace);
        let verb = parts.next().unwrap_or("");
        let rest = parts.next().unwrap_or("").trim();

        match verb {
            "ACQUIRE" => {
                if rest.is_empty() {
                    reply(stream, "ERR ACQUIRE needs a client-id");
                    continue;
                }
                let s = match stream.try_clone() {
                    Ok(s) => s,
                    Err(_) => return,
                };
                *held_id = Some(rest.to_string());
                if tx
                    .send(Cmd::Acquire { id: rest.to_string(), stream: s })
                    .is_err()
                {
                    return; // worker gone
                }
                // Keep the connection open: the worker replies GRANTED here and
                // we block on the next read_line until RELEASE or EOF.
            }
            "RELEASE" => {
                if rest.is_empty() {
                    reply(stream, "ERR RELEASE needs a client-id");
                    continue;
                }
                if tx.send(Cmd::Release { id: rest.to_string() }).is_err() {
                    return;
                }
                *held_id = None;
            }
            "STATUS" => {
                let s = match stream.try_clone() {
                    Ok(s) => s,
                    Err(_) => return,
                };
                let _ = tx.send(Cmd::Status { stream: s });
                return; // worker replies, connection is single-shot.
            }
            "STOP" => {
                let s = match stream.try_clone() {
                    Ok(s) => s,
                    Err(_) => return,
                };
                let _ = tx.send(Cmd::Stop { stream: s });
                return;
            }
            _ => {
                reply(stream, "ERR unknown verb");
            }
        }
    }
}

/// `git zdaemon status` — one-shot STATUS query. Not running is not an error.
fn status() -> Result<ExitCode> {
    let path = socket_path();
    match query(&path, "STATUS") {
        Some(resp) => println!("{resp}"),
        None => println!("not running"),
    }
    Ok(ExitCode::SUCCESS)
}

/// `git zdaemon stop` — ask the daemon to exit, then reap the socket. Not
/// running is not an error.
fn stop() -> Result<ExitCode> {
    let path = socket_path();
    match query(&path, "STOP") {
        Some(resp) => {
            println!("{resp}");
            // The daemon removes the socket on STOP; sweep any residue.
            let _ = std::fs::remove_file(&path);
        }
        None => {
            println!("not running");
            // Clean up a stale socket file if one was left behind.
            let _ = std::fs::remove_file(&path);
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Connect, send a single request line, and return the first reply line.
/// `None` means the daemon is not reachable.
fn query(path: &Path, request: &str) -> Option<String> {
    let mut stream = UnixStream::connect(path).ok()?;
    // A live daemon answers promptly; don't hang a CLI forever on a wedged one.
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    stream.write_all(request.as_bytes()).ok()?;
    stream.write_all(b"\n").ok()?;
    stream.flush().ok()?;

    let mut reader = BufReader::new(&stream);
    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(0) => None,
        Ok(_) => Some(line.trim_end().to_string()),
        Err(_) => None,
    }
}
