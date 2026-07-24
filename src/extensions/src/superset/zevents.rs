//! `git zevents` (alias `git ztail`) — one structured live feed of commits,
//! reconciles, and status-changes across the whole tree.
//!
//! Events come from the index's append-only `events` table. `commit` and `status`
//! rows are written by triggers on `repo_status`, which is maintained by BOTH the
//! whole-tree status poller (statusd) and the instant file-watcher (watch) — so a
//! HEAD move anywhere becomes a `commit` event and a dirty/sync transition becomes
//! a `status` event, with no per-call plumbing. `reconcile` rows are recorded by
//! the daemon's autonomy path when a fetch-free fast-forward actually advances a
//! repo. This verb prints a short backlog, then follows live.
//!
//! Usage:
//!   git ztail                     backlog + live follow (all repos, all kinds)
//!   git zevents -n 50             show the last 50 before following
//!   git zevents --no-follow       print the backlog and exit (no live tail)
//!   git zevents --kind commit     only commits (also: status, reconcile)
//!   git zevents --repo zpwr       only repos whose path contains "zpwr"
//!   git zevents --json            NDJSON, one event per line (for tooling)

use crate::db::EventRow;
use anyhow::Result;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

const RESET: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const MAGENTA: &str = "\x1b[35m";

/// How often the live follow polls for new events.
const POLL: Duration = Duration::from_millis(400);

struct Opts {
    limit: usize,
    follow: bool,
    json: bool,
    kind: Option<String>,
    repo: Option<String>,
}

fn parse(args: &[String]) -> Opts {
    let mut o = Opts { limit: 20, follow: true, json: false, kind: None, repo: None };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-n" | "--lines" => {
                if let Some(v) = it.next() {
                    o.limit = v.parse().unwrap_or(o.limit);
                }
            }
            "--no-follow" | "-1" => o.follow = false,
            "--json" => o.json = true,
            "--kind" | "-k" => o.kind = it.next().cloned(),
            "--repo" | "-r" => o.repo = it.next().cloned(),
            _ => {}
        }
    }
    o
}

pub fn zevents(args: &[String]) -> Result<ExitCode> {
    let o = parse(args);
    let conn = crate::db::open_rw()?;

    // Backlog: the most recent `limit` events (oldest-first), then follow anything
    // that arrives after. Track the highest id seen so the follow never repeats.
    let backlog = crate::db::events_recent(&conn, o.limit, o.kind.as_deref(), o.repo.as_deref())?;
    let mut last = 0i64;
    for e in &backlog {
        emit(e, o.json);
        last = last.max(e.id);
    }

    if !o.follow {
        return Ok(ExitCode::SUCCESS);
    }
    // Empty backlog → follow from the current tip, so we don't replay history the
    // filters happened to exclude.
    if last == 0 {
        last = crate::db::max_event_id(&conn);
    }
    if !o.json {
        eprintln!("{DIM}live feed — commits · reconciles · status — ctrl-c to stop{RESET}");
    }
    loop {
        let batch = crate::db::events_since(&conn, last, o.kind.as_deref(), o.repo.as_deref())?;
        for e in &batch {
            emit(e, o.json);
            last = last.max(e.id);
        }
        std::thread::sleep(POLL);
    }
}

/// Print one event — a colored human line, or one NDJSON object with `--json`.
fn emit(e: &EventRow, json: bool) {
    if json {
        let obj = serde_json::json!({
            "id": e.id,
            "ts": e.ts,
            "kind": e.kind,
            "repo": repo_path(e),
            "detail": e.detail,
            "before": e.sha_before,
            "after": e.sha_after,
        });
        println!("{obj}");
        return;
    }
    let (glyph, label) = match e.kind.as_str() {
        "commit" => (format!("{GREEN}●{RESET}"), format!("{GREEN}commit  {RESET}")),
        "reconcile" => (format!("{MAGENTA}↻{RESET}"), format!("{MAGENTA}reconcile{RESET}")),
        "status" => (format!("{YELLOW}◑{RESET}"), format!("{YELLOW}status  {RESET}")),
        other => ("•".to_string(), format!("{other:<9}")),
    };
    println!(
        "{DIM}{}{RESET} {glyph} {label} {CYAN}{:<22}{RESET} {}",
        hms(e.ts),
        repo_label(e),
        detail(e),
    );
}

/// The change described, per kind: sha delta for a commit, the message for a
/// reconcile, the new state for a status change.
fn detail(e: &EventRow) -> String {
    match e.kind.as_str() {
        "commit" => format!(
            "{}{DIM}→{RESET}{}",
            short(e.sha_before.as_deref()),
            short(e.sha_after.as_deref())
        ),
        _ => e.detail.clone().unwrap_or_default(),
    }
}

fn short(sha: Option<&str>) -> String {
    match sha {
        Some(s) if s.len() >= 10 => s[..10].to_string(),
        Some(s) => s.to_string(),
        None => "·".to_string(),
    }
}

/// Full repo path for `--json` (workdir preferred, else the git dir).
fn repo_path(e: &EventRow) -> Option<String> {
    e.workdir.clone().or_else(|| e.git_dir.clone())
}

/// Short repo label for the human feed: the working-tree directory name, or the
/// repo name derived from a bare/`.git` path.
fn repo_label(e: &EventRow) -> String {
    if let Some(w) = &e.workdir {
        if let Some(n) = Path::new(w).file_name() {
            return n.to_string_lossy().into_owned();
        }
    }
    if let Some(g) = &e.git_dir {
        let p = Path::new(g);
        // ".../<repo>/.git" → "<repo>"; a bare "....git" → its own stem.
        let base = if p.file_name().map(|n| n == ".git").unwrap_or(false) {
            p.parent().and_then(|q| q.file_name())
        } else {
            p.file_name()
        };
        if let Some(n) = base {
            return n.to_string_lossy().trim_end_matches(".git").to_string();
        }
    }
    "?".to_string()
}

/// Local-time `HH:MM:SS` for an epoch-seconds timestamp (via libc, no tz crate).
fn hms(ts: i64) -> String {
    let t = ts as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return "--:--:--".to_string();
    }
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}
