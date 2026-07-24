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
//! Socket path: `~/.zvcs/zvcs.sock` (override with `ZVCS_SOCK`; a `ZVCS_HOME`
//! too deep to fit `sun_path` falls back to a short, stable `/tmp` socket).
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
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

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

/// A unix-domain socket path must fit the platform's `sockaddr_un.sun_path`
/// array (incl. its NUL terminator): 104 bytes on macOS/BSD, 108 on Linux. Use
/// the tighter limit so the overflow fallback below triggers identically on
/// every OS. `bind()` fails when the path length reaches this bound.
const SUN_PATH_MAX: usize = 104;

/// The singleton socket path: `$ZVCS_SOCK`, else `~/.zvcs/zvcs.sock`, except a
/// derived default that would overflow [`SUN_PATH_MAX`] (a deep `ZVCS_HOME`,
/// e.g. a nested scratchpad) falls back to a short, stable `/tmp` path keyed on
/// the home dir — otherwise `bind()` fails with "path must be shorter than
/// SUN_LEN" and the daemon can never start. An explicit `$ZVCS_SOCK` is always
/// honored verbatim (the caller owns its length).
pub fn socket_path() -> PathBuf {
    if let Some(s) = std::env::var_os("ZVCS_SOCK") {
        return PathBuf::from(s);
    }
    let home = zvcs_home();
    let default = home.join("zvcs.sock");
    if default.as_os_str().len() < SUN_PATH_MAX {
        return default;
    }
    short_fallback_socket(&home)
}

/// A short `/tmp` socket path derived from `home`, for when the default under
/// `ZVCS_HOME` is too long to bind. Deterministic (same binary → same digest via
/// the fixed-seed `DefaultHasher`), so the daemon and every client compute the
/// same path, and keyed on `home` so isolated daemons (distinct `ZVCS_HOME`s)
/// never collide on one socket.
fn short_fallback_socket(home: &Path) -> PathBuf {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    home.hash(&mut hasher);
    PathBuf::from(format!("/tmp/zvcs-{:016x}.sock", hasher.finish()))
}

/// Best-effort single-line reply. Never panics.
fn reply(mut stream: &UnixStream, msg: &str) {
    let _ = stream.write_all(msg.as_bytes());
    let _ = stream.write_all(b"\n");
    let _ = stream.flush();
}

const USAGE: &str = "usage: git zdaemon <start|stop|restart|reload|status|info|ping|log>";

pub fn zdaemon(args: &[String]) -> Result<ExitCode> {
    let action = args.first().map(String::as_str).unwrap_or("");
    match action {
        "start" => start(),
        "stop" => stop(),
        "restart" | "reload" => restart(),
        "status" => status(),
        "info" => info(),
        "ping" => ping_cmd(),
        "log" => log_cmd(&args[1..]),
        "" => anyhow::bail!("{USAGE}"),
        other => anyhow::bail!("unknown action {other:?} — {USAGE}"),
    }
}

/// The daemon's pidfile, written on start and removed on stop.
fn pid_path() -> PathBuf {
    zvcs_home().join("zvcs.pid")
}

/// Marker a manual `zdaemon stop` writes to keep the daemon down. Autostart
/// checks it ([`autostart_disabled`]) and will NOT respawn while it exists;
/// `start`/`restart` remove it, so an explicit bring-up re-enables autostart.
/// Without this a manual stop is futile — the next `git` command, across up to 16
/// concurrent instances, autostarts the daemon straight back up.
fn autostart_disabled_path() -> PathBuf {
    zvcs_home().join("zdaemon.disabled")
}

/// Whether a manual `zdaemon stop` has disabled autostart. Read by
/// [`crate::autostart::ensure_if_configured`] before it would spawn a daemon.
pub fn autostart_disabled() -> bool {
    autostart_disabled_path().exists()
}

/// Probe whether a live daemon owns the socket.
fn ping(path: &Path) -> bool {
    match UnixStream::connect(path) {
        Ok(mut stream) => {
            // Bound the wait: a listener whose worker thread has died would accept
            // the connection but never answer, hanging `read_line` forever.
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
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

/// Best-effort exclusive lock across the start sequence. Two concurrent starters
/// (the 16-instance workload autostarts freely) must not both unlink a socket and
/// bind their own — that leaves two live daemons (duplicated autonomy + a leaked
/// process). Held across check-remove-bind. Polls to acquire for up to 6s (longer
/// than the worst-case guarded section) so a live-but-slow holder is waited for,
/// not stolen from; only a lock left by a hard-killed starter (held past the
/// window) is stolen, so a crash can't wedge startup permanently. Returns None if
/// even the steal fails (then we proceed anyway — no worse than an unlocked start).
fn hold_start_lock(at: &Path) -> Option<gix::lock::Marker> {
    use gix::lock::{acquire::Fail, Marker};
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        if let Ok(m) = Marker::acquire_to_hold_resource(at, Fail::Immediately, None) {
            return Some(m);
        }
        if Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    // Held past the window → treat as a crashed starter's stale lock and steal it.
    let mut stale = at.as_os_str().to_owned();
    stale.push(".lock");
    let _ = std::fs::remove_file(PathBuf::from(stale));
    Marker::acquire_to_hold_resource(at, Fail::Immediately, None).ok()
}

/// Whether a daemon is actually serving on `path`. `connect` distinguishes a stale
/// socket *file* (whose daemon died → connect refused) from a bound one (live, or
/// mid-startup with the request sitting in the listen backlog). A bound socket is
/// NEVER treated as stale: removing it would let this starter bind a second daemon
/// while the first is still coming up. Only a socket that stays connectable but
/// never answers STATUS within the window is a zombie worth replacing.
fn socket_is_live(path: &Path) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if UnixStream::connect(path).is_err() {
            return false; // nothing bound → stale/absent, safe to remove
        }
        if ping(path) {
            return true; // bound AND answering → a real daemon
        }
        if Instant::now() >= deadline {
            return false; // bound but never answered → zombie worker, take over
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn start() -> Result<ExitCode> {
    let path = socket_path();
    // An explicit start re-enables autostart, clearing any prior manual stop.
    let _ = std::fs::remove_file(autostart_disabled_path());

    let listener = {
        let _marker = hold_start_lock(&zvcs_home().join("daemon-start"));
        if path.exists() {
            // Only remove a socket that is genuinely dead. A still-starting daemon
            // is bound (connectable) but may not answer STATUS for a moment;
            // reply-based staleness would falsely remove it → two live daemons.
            if socket_is_live(&path) {
                anyhow::bail!("daemon already running");
            }
            let _ = std::fs::remove_file(&path);
        }
        UnixListener::bind(&path)
            .map_err(|e| anyhow::anyhow!("cannot bind {}: {e}", path.display()))?
    };

    // Record our pid for `zdaemon info`.
    let _ = std::fs::write(pid_path(), std::process::id().to_string());

    let (tx, rx): (Sender<Cmd>, Receiver<Cmd>) = mpsc::channel();

    let worker_path = path.clone();
    thread::spawn(move || worker_loop(rx, worker_path));

    // Bounded async-job pool (zcommit/zpush execution). Concurrency = cores,
    // capped so a burst can't spawn unbounded threads.
    let workers = std::thread::available_parallelism()
        .map(|n| n.get().min(8))
        .unwrap_or(4);
    crate::jobpool::init(workers);

    // Reactive autonomy: file-watcher driven, never polled (§ watch). Spawns
    // nothing unless `[zvcs]` autonomy is configured for the discovered repo.
    crate::superset::watch::spawn_if_configured();

    // Background repo crawl on start, iff `[zvcs] autocrawl` is enabled.
    crate::crawler::spawn_if_configured();

    // Background status maintainer: the worker pool keeps repo_status warm for
    // every indexed repo, so `zdashboard` / `zstatus --all` are instant AND
    // accurate without needing `[zvcs] autostatus`. Tunable via
    // `zvcs.statusinterval` (seconds between passes; 0 disables).
    crate::superset::statusd::spawn_if_enabled();

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
                let _ = std::fs::remove_file(pid_path());
                std::process::exit(0);
            }
        }
    }
}

/// Per-connection reader. On EOF/error it auto-releases whatever lock this
/// connection acquired so a crashed holder can never wedge its repo.
fn handle_conn(stream: UnixStream, tx: Sender<Cmd>) {
    // A connection may acquire more than one lane; EOF must release *every* one,
    // or an un-released lane wedges its repo forever. (A scalar released only the
    // last acquire — the exact wedge the FIFO design exists to prevent.)
    let mut held: Vec<(PathBuf, String)> = Vec::new();
    conn_loop(&stream, &tx, &mut held);
    for (repo, id) in held {
        let _ = tx.send(Cmd::Release { repo, id });
    }
}

fn conn_loop(stream: &UnixStream, tx: &Sender<Cmd>, held: &mut Vec<(PathBuf, String)>) {
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
                held.push((repo.clone(), id.clone()));
                if tx.send(Cmd::Acquire { repo, id, stream: s }).is_err() {
                    return;
                }
            }
            Req::Release { id: wire_id } => {
                // Release the (repo, id) this connection actually acquired — not
                // blindly the id echoed on the wire — so an explicit RELEASE
                // behaves like the EOF auto-release path. Prefer the entry whose
                // id matches the wire; fall back to the sole hold when there is
                // exactly one (a lone RELEASE always means "the one I hold").
                let pos = held
                    .iter()
                    .position(|(_, hid)| *hid == wire_id)
                    .or(if held.len() == 1 { Some(0) } else { None });
                if let Some(p) = pos {
                    let (repo, held_id) = held.remove(p);
                    if tx.send(Cmd::Release { repo, id: held_id }).is_err() {
                        return;
                    }
                }
            }
            Req::Submit(json) => {
                match handle_submit(&json) {
                    Some(id) => reply(stream, &format!("JOB {id}")),
                    None => reply(stream, "ERR could not queue job"),
                }
                return; // single-shot
            }
            Req::JobStop(id) => {
                if crate::jobpool::stop(id) {
                    reply(stream, "OK");
                } else {
                    reply(stream, &format!("ERR no stoppable job #{id}"));
                }
                return;
            }
            Req::JobRestart(id) => {
                match handle_restart(id) {
                    Some(new_id) => reply(stream, &format!("JOB {new_id}")),
                    None => reply(stream, &format!("ERR no job #{id}")),
                }
                return;
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
    JobStop(i64),
    JobRestart(i64),
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
        "JOBSTOP" => match rest.parse() {
            Ok(id) => Req::JobStop(id),
            Err(_) => Req::Err("JOBSTOP needs a job id".into()),
        },
        "JOBRESTART" => match rest.parse() {
            Ok(id) => Req::JobRestart(id),
            Err(_) => Req::Err("JOBRESTART needs a job id".into()),
        },
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

    // Hand to the bounded pool. The child porcelain each job spawns acquires its
    // repo's lane itself, so ordering is preserved; the pool caps concurrency and
    // makes the job cancellable via `zjob stop`.
    crate::jobpool::submit(id, json.to_string());
    Some(id)
}

/// Restart a finished/failed job: clone it (parent-linked) and enqueue the copy.
/// Returns the new job id.
fn handle_restart(id: i64) -> Option<i64> {
    let conn = crate::db::open_rw().ok()?;
    let (new_id, spec) = crate::db::restart_job(&conn, id).ok()??;
    drop(conn);
    crate::jobpool::submit(new_id, spec);
    Some(new_id)
}

/// `git zdaemon status` — one-shot STATUS query. Not running is not an error.
fn status() -> Result<ExitCode> {
    match query(&socket_path(), "STATUS") {
        Some(resp) => println!("{resp}"),
        None if autostart_disabled() => {
            println!("not running (autostart disabled — `git zdaemon start` to re-enable)")
        }
        None => println!("not running"),
    }
    Ok(ExitCode::SUCCESS)
}

/// `git zdaemon stop` — ask the daemon to exit, then reap the socket + pidfile.
/// Also sets the autostart-disable marker so the daemon stays down: otherwise the
/// next `git` command (this instance or any concurrent one) would autostart it
/// straight back up. `git zdaemon start`/`restart` clears the marker.
fn stop() -> Result<ExitCode> {
    let path = socket_path();
    // Disable autostart BEFORE stopping, so a `git` command racing in during the
    // stop cannot respawn the daemon we are about to kill.
    let _ = std::fs::create_dir_all(zvcs_home());
    let _ = std::fs::write(autostart_disabled_path(), b"");
    match query(&path, "STOP") {
        Some(resp) => println!("{resp}"),
        None => println!("not running"),
    }
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(pid_path());
    Ok(ExitCode::SUCCESS)
}

/// `git zdaemon restart` (alias `reload`) — stop any running daemon, then respawn
/// it detached (re-reading `[zvcs]` config and rebuilding the watch set).
fn restart() -> Result<ExitCode> {
    let path = socket_path();
    // A restart is an explicit bring-up: re-enable autostart (clear a prior stop).
    let _ = std::fs::remove_file(autostart_disabled_path());
    if ping(&path) {
        let _ = query(&path, "STOP");
    }
    // Wait for the old daemon to release the socket.
    for _ in 0..50 {
        if !path.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(40));
    }
    // Only unlink a *stale* socket. If a daemon is still answering (STOP was slow
    // or failed), removing its socket would orphan it and a fresh daemon would
    // bind empty lane state — split-brain, two holders for one repo. Abort instead.
    if ping(&path) {
        anyhow::bail!("daemon still running (STOP did not take); not restarting");
    }
    let _ = std::fs::remove_file(&path);

    let workdir = gix::discover(".")
        .ok()
        .and_then(|r| r.workdir().map(|w| w.to_path_buf()))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    crate::autostart::spawn_detached(&workdir);

    for _ in 0..75 {
        if ping(&path) {
            println!("restarted");
            return Ok(ExitCode::SUCCESS);
        }
        thread::sleep(Duration::from_millis(40));
    }
    println!("restart: daemon did not come up (see {})", zvcs_home().join("zvcs.log").display());
    Ok(ExitCode::FAILURE)
}

/// `git zdaemon ping` — exit 0 if a daemon is live, 1 otherwise (scriptable).
fn ping_cmd() -> Result<ExitCode> {
    if ping(&socket_path()) {
        println!("running");
        Ok(ExitCode::SUCCESS)
    } else {
        println!("not running");
        Ok(ExitCode::FAILURE)
    }
}

/// `git zdaemon log [-n N] [-f]` — show (and optionally follow) the daemon log.
fn log_cmd(args: &[String]) -> Result<ExitCode> {
    let mut n: usize = 40;
    let mut follow = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-n" => {
                i += 1;
                n = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(n);
            }
            "-f" | "--follow" => follow = true,
            _ => {}
        }
        i += 1;
    }
    let log = zvcs_home().join("zvcs.log");
    // Lossy decode: a subprocess could have written a non-UTF-8 byte to the log,
    // which would make `read_to_string` error and drop the whole file.
    let content = std::fs::read(&log)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    let lines: Vec<&str> = content.lines().collect();
    let start = lines.len().saturating_sub(n);
    for l in &lines[start..] {
        println!("{l}");
    }
    if follow {
        // Simple tail -f: print bytes appended after our current position.
        let mut pos = content.len() as u64;
        loop {
            thread::sleep(Duration::from_millis(400));
            let Ok(meta) = std::fs::metadata(&log) else { continue };
            let len = meta.len();
            if len > pos {
                if let Ok(mut f) = std::fs::File::open(&log) {
                    use std::io::{Read, Seek, SeekFrom};
                    if f.seek(SeekFrom::Start(pos)).is_ok() {
                        let mut buf = String::new();
                        if f.read_to_string(&mut buf).is_ok() {
                            print!("{buf}");
                            let _ = std::io::Write::flush(&mut std::io::stdout());
                        }
                    }
                }
                pos = len;
            } else if len < pos {
                pos = len; // log rotated/truncated
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `git zdaemon info` — running state, paths, pid, live lane snapshot, config.
fn info() -> Result<ExitCode> {
    let sock = socket_path();
    let running = ping(&sock);
    println!("running: {running}");
    if let Ok(pid) = std::fs::read_to_string(pid_path()) {
        println!("pid:     {}", pid.trim());
    }
    println!("socket:  {}", sock.display());
    println!("home:    {}", zvcs_home().display());
    println!("log:     {}", zvcs_home().join("zvcs.log").display());
    println!("db:      {}", crate::db::db_path().display());
    if let Some(state) = query(&sock, "STATUS") {
        println!("state:   {state}");
    }
    if let Ok(repo) = gix::discover(".") {
        let cfg = crate::config::ZvcsConfig::load(&repo);
        println!(
            "config:  autoreconcile={} autobump={} autocrawl={} autostatus={} hook={} interval={}s",
            cfg.autoreconcile,
            cfg.autobump,
            cfg.autocrawl,
            cfg.autostatus,
            cfg.hook.is_some(),
            cfg.interval.as_secs()
        );
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
