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
        None => println!("not running"),
    }
    Ok(ExitCode::SUCCESS)
}

/// `git zdaemon stop` — ask the daemon to exit, then reap the socket + pidfile.
fn stop() -> Result<ExitCode> {
    let path = socket_path();
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
