//! `git zdaemon <start|stop|status>` — the **singleton** fair-mutex coordinator.
//!
//! One daemon per machine (state under `~/.zvcs/`), not one per repo. It replaces
//! git's `index.lock` — which fails fast under contention
//! (`fatal: Unable to create '.git/index.lock': File exists`) — with a fair
//! userspace FIFO, **per repository**. A contended `ACQUIRE` blocks in its repo's
//! queue and is answered `GRANTED` only when its turn comes, instead of failing.
//!
//! Per-repo lanes are the crux: submodule `foo` and submodule `bar` have separate
//! indexes, so their writers must never serialize against each other. The worker
//! keeps a `HashMap<RepoKey, Lane>`; unrelated repos run fully in parallel, and
//! only writers to the *same* repo queue.
//!
//! # Wire protocol (line-based, one request per line, over the unix socket)
//!
//! Socket path: `~/.zvcs/zvcs.sock` (override with `ZVCS_SOCK`).
//!
//! Client -> daemon:
//!   * `ACQUIRE <client-id> <git-dir>` — enqueue a lock request for the repo at
//!     `<git-dir>` (which may contain spaces; it is the remainder of the line).
//!     Answered `GRANTED` on this stream when the caller reaches the head of that
//!     repo's FIFO. The connection stays open while the lock is held.
//!   * `RELEASE <client-id>` — the current holder releases; the daemon grants the
//!     next queued waiter for that repo.
//!   * `STATUS` — one line `holder=<id|none> lanes=<n>`, then the connection closes.
//!   * `STOP` — `STOPPING`, remove the socket, exit.
//!
//! # Crash-holder auto-release
//!
//! The holder keeps its `ACQUIRE` connection open. If it crashes, the socket hits
//! EOF, the connection thread synthesizes a `RELEASE` for the (repo, id) it held,
//! and the worker promotes the next waiter. A crashed holder cannot wedge a repo.

use anyhow::Result;
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

/// Requests handed from per-connection reader threads to the single worker
/// thread that owns all critical sections.
enum Cmd {
    Acquire {
        repo: PathBuf,
        id: String,
        stream: UnixStream,
    },
    Release {
        repo: PathBuf,
        id: String,
    },
    Status {
        stream: UnixStream,
    },
    Stop {
        stream: UnixStream,
    },
}

/// One repository's critical section: the current holder and its FIFO of waiters.
#[derive(Default)]
struct Lane {
    holder: Option<String>,
    queue: VecDeque<(String, UnixStream)>,
}

/// The zvcs state directory, `~/.zvcs` (override with `ZVCS_HOME`). Created on
/// demand; falls back to the current directory if `$HOME` is unset.
pub fn zvcs_home() -> PathBuf {
    let dir = std::env::var_os("ZVCS_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".zvcs")))
        .unwrap_or_else(|| PathBuf::from(".zvcs"));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// The singleton socket path: `$ZVCS_SOCK`, else `~/.zvcs/zvcs.sock`.
pub fn socket_path() -> PathBuf {
    if let Some(s) = std::env::var_os("ZVCS_SOCK") {
        return PathBuf::from(s);
    }
    zvcs_home().join("zvcs.sock")
}

/// Best-effort single-line reply. Never panics.
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

/// Probe whether a live daemon owns the socket.
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
        let _ = std::fs::remove_file(&path);
    }

    let listener = UnixListener::bind(&path)
        .map_err(|e| anyhow::anyhow!("cannot bind {}: {e}", path.display()))?;

    let (tx, rx): (Sender<Cmd>, Receiver<Cmd>) = mpsc::channel();

    let worker_path = path.clone();
    thread::spawn(move || worker_loop(rx, worker_path));

    // Reactive autonomy: file-watcher driven, never polled (§ watch). Spawns
    // nothing unless `[zvcs]` autonomy is configured for the discovered repo.
    crate::superset::watch::spawn_if_configured();

    for incoming in listener.incoming() {
        let stream = match incoming {
            Ok(s) => s,
            Err(_) => continue,
        };
        let tx = tx.clone();
        thread::spawn(move || handle_conn(stream, tx));
    }

    Ok(ExitCode::SUCCESS)
}

/// The critical-section owner: grants each repo's lock to one holder at a time in
/// strict FIFO order, with unrelated repos fully independent.
fn worker_loop(rx: Receiver<Cmd>, sock_path: PathBuf) {
    let mut lanes: HashMap<PathBuf, Lane> = HashMap::new();

    while let Ok(cmd) = rx.recv() {
        match cmd {
            Cmd::Acquire { repo, id, stream } => {
                let lane = lanes.entry(repo).or_default();
                if lane.holder.is_none() {
                    reply(&stream, "GRANTED");
                    lane.holder = Some(id);
                } else {
                    lane.queue.push_back((id, stream));
                }
            }
            Cmd::Release { repo, id } => {
                if let Some(lane) = lanes.get_mut(&repo) {
                    if lane.holder.as_deref() == Some(id.as_str()) {
                        if let Some((next_id, next_stream)) = lane.queue.pop_front() {
                            reply(&next_stream, "GRANTED");
                            lane.holder = Some(next_id);
                        } else {
                            // Lane idle: drop it so memory tracks *active* repos,
                            // not every repo ever touched.
                            lanes.remove(&repo);
                        }
                    } else {
                        // A queued waiter cancelled (crashed before its turn).
                        lane.queue.retain(|(qid, _)| qid != &id);
                        if lane.holder.is_none() && lane.queue.is_empty() {
                            lanes.remove(&repo);
                        }
                    }
                }
            }
            Cmd::Status { stream } => {
                let holder = lanes
                    .values()
                    .find_map(|l| l.holder.clone())
                    .unwrap_or_else(|| "none".to_string());
                reply(&stream, &format!("holder={} lanes={}", holder, lanes.len()));
            }
            Cmd::Stop { stream } => {
                reply(&stream, "STOPPING");
                let _ = std::fs::remove_file(&sock_path);
                std::process::exit(0);
            }
        }
    }
}

/// Per-connection reader. On EOF/error it auto-releases whatever lock this
/// connection acquired so a crashed holder can never wedge its repo.
fn handle_conn(stream: UnixStream, tx: Sender<Cmd>) {
    let mut held: Option<(PathBuf, String)> = None;
    conn_loop(&stream, &tx, &mut held);
    if let Some((repo, id)) = held {
        let _ = tx.send(Cmd::Release { repo, id });
    }
}

fn conn_loop(stream: &UnixStream, tx: &Sender<Cmd>, held: &mut Option<(PathBuf, String)>) {
    let reader_stream = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut reader = BufReader::new(reader_stream);
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return,
            Ok(_) => {}
            Err(_) => return,
        }

        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.trim().is_empty() {
            continue;
        }

        match parse(trimmed) {
            Req::Acquire { id, repo } => {
                let s = match stream.try_clone() {
                    Ok(s) => s,
                    Err(_) => return,
                };
                *held = Some((repo.clone(), id.clone()));
                if tx.send(Cmd::Acquire { repo, id, stream: s }).is_err() {
                    return;
                }
            }
            Req::Release { id } => {
                if let Some((repo, _)) = held.clone() {
                    if tx.send(Cmd::Release { repo, id }).is_err() {
                        return;
                    }
                }
                *held = None;
            }
            Req::Submit(json) => {
                match handle_submit(&json) {
                    Some(id) => reply(stream, &format!("JOB {id}")),
                    None => reply(stream, "ERR could not queue job"),
                }
                return; // single-shot
            }
            Req::Status => {
                if let Ok(s) = stream.try_clone() {
                    let _ = tx.send(Cmd::Status { stream: s });
                }
                return;
            }
            Req::Stop => {
                if let Ok(s) = stream.try_clone() {
                    let _ = tx.send(Cmd::Stop { stream: s });
                }
                return;
            }
            Req::Err(msg) => reply(stream, &format!("ERR {msg}")),
        }
    }
}

enum Req {
    Acquire { id: String, repo: PathBuf },
    Release { id: String },
    Submit(String),
    Status,
    Stop,
    Err(String),
}

/// Parse one request line. `ACQUIRE <id> <git-dir>` keeps the git-dir as the
/// remainder so it may contain spaces; `id` never does (`<pid>-<seq>`).
fn parse(line: &str) -> Req {
    let mut it = line.splitn(2, char::is_whitespace);
    let verb = it.next().unwrap_or("");
    let rest = it.next().unwrap_or("").trim();
    match verb {
        "ACQUIRE" => {
            let mut p = rest.splitn(2, char::is_whitespace);
            let id = p.next().unwrap_or("").trim();
            let repo = p.next().unwrap_or("").trim();
            if id.is_empty() || repo.is_empty() {
                Req::Err("ACQUIRE needs <client-id> <git-dir>".into())
            } else {
                Req::Acquire {
                    id: id.to_string(),
                    repo: PathBuf::from(repo),
                }
            }
        }
        "RELEASE" => {
            if rest.is_empty() {
                Req::Err("RELEASE needs a client-id".into())
            } else {
                Req::Release { id: rest.to_string() }
            }
        }
        "SUBMIT" => {
            if rest.is_empty() {
                Req::Err("SUBMIT needs a job spec".into())
            } else {
                Req::Submit(rest.to_string())
            }
        }
        "STATUS" => Req::Status,
        "STOP" => Req::Stop,
        other => Req::Err(format!("unknown verb {other:?}")),
    }
}

/// Queue an async job: record it, spawn its executor off the lock worker, and
/// return the job id. Returns `None` if the spec is unusable or the ledger is
/// unavailable.
fn handle_submit(json: &str) -> Option<i64> {
    let spec: serde_json::Value = serde_json::from_str(json).ok()?;
    let git_dir = spec.get("git_dir").and_then(|v| v.as_str())?;
    let kind = spec.get("kind").and_then(|v| v.as_str())?;
    let workdir = spec.get("workdir").and_then(|v| v.as_str());
    let session = spec.get("session").and_then(|v| v.as_str());

    let conn = crate::db::open_rw().ok()?;
    let repo_id = crate::db::upsert_repo(
        &conn,
        Path::new(git_dir),
        workdir.map(Path::new),
    )
    .ok()?;
    let id = crate::db::insert_job(&conn, repo_id, kind, json, session).ok()?;
    drop(conn);

    // Execute off the connection/worker threads. The child porcelain the job
    // spawns will acquire its repo's lane itself, so ordering is preserved.
    let json_owned = json.to_string();
    thread::spawn(move || {
        if let Ok(conn) = crate::db::open_rw() {
            let _ = crate::db::job_running(&conn, id);
        }
        let spec: serde_json::Value = match serde_json::from_str(&json_owned) {
            Ok(v) => v,
            Err(_) => return,
        };
        let result = crate::jobrun::execute(&spec);
        if let Ok(conn) = crate::db::open_rw() {
            let state = if result.ok { "done" } else { "failed" };
            let exit = if result.ok { 0 } else { 1 };
            let _ = crate::db::job_finished(
                &conn,
                id,
                state,
                exit,
                &result.output,
                result.sha_after.as_deref(),
            );
        }
    });
    Some(id)
}

/// `git zdaemon status` — one-shot STATUS query. Not running is not an error.
fn status() -> Result<ExitCode> {
    match query(&socket_path(), "STATUS") {
        Some(resp) => println!("{resp}"),
        None => println!("not running"),
    }
    Ok(ExitCode::SUCCESS)
}

/// `git zdaemon stop` — ask the daemon to exit, then reap the socket.
fn stop() -> Result<ExitCode> {
    let path = socket_path();
    match query(&path, "STOP") {
        Some(resp) => {
            println!("{resp}");
            let _ = std::fs::remove_file(&path);
        }
        None => {
            println!("not running");
            let _ = std::fs::remove_file(&path);
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Connect, send one request line, return the first reply line.
fn query(path: &Path, request: &str) -> Option<String> {
    let mut stream = UnixStream::connect(path).ok()?;
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
