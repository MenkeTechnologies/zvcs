//! Directory triggers — `git ztrigger <DIR> <command>` and `git zwatch <DIR>`.
//!
//! A general "watch this directory, run this command on any change" mechanism,
//! independent of git: the DIR does **not** have to be a repository. Triggers are
//! keyed by canonical directory path in the index's `triggers` table; the daemon
//! watches each path recursively and runs its command the instant a file under it
//! changes (via `sh -c`, cwd = the dir, `$ZVCS_DIR` set).
//!
//! Because one file action emits several filesystem events, each trigger has a
//! **leading-edge throttle** (default 500ms): the first event fires immediately,
//! then further events are coalesced for the throttle window — so a save fires
//! once, not five times. Override with `--throttle <dur>` (`0` = every event).
//!
//! Live views onto the daemon's fires (recorded to `~/.zvcs/fires.log`):
//!   * `git ztrigger tail` — stream each fire as it happens;
//!   * `git ztrigger top`  — an in-place HUD of per-trigger fire counts and rate.

use anyhow::{anyhow, bail, Result};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

// ---- ANSI (kept minimal; values are colored, layout stays plain) -----------
const RESET: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[32m";
const RED: &str = "\x1b[31m";
const YELLOW: &str = "\x1b[33m";

/// Resolve a directory argument to its canonical path, requiring that it exists
/// and is a directory. Any directory is valid — no git repository required.
fn resolve_dir(dir: &str) -> Result<PathBuf> {
    let canon = PathBuf::from(dir).canonicalize().map_err(|e| anyhow!("{dir}: {e}"))?;
    if !canon.is_dir() {
        bail!("{dir}: not a directory");
    }
    Ok(canon)
}

/// Parse a throttle duration: `500ms`, `3s`, `2m`, or a bare number (seconds).
fn parse_throttle(s: &str) -> Result<i64> {
    let s = s.trim();
    let (num, mult) = if let Some(n) = s.strip_suffix("ms") {
        (n, 1)
    } else if let Some(n) = s.strip_suffix('s') {
        (n, 1000)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 60_000)
    } else {
        (s, 1000)
    };
    let v: f64 = num.trim().parse().map_err(|_| anyhow!("bad duration: {s}"))?;
    if v < 0.0 {
        bail!("throttle must be >= 0");
    }
    Ok((v * mult as f64) as i64)
}

/// `git ztrigger <DIR <cmd>... [--throttle <dur>] | list | rm DIR | test DIR | tail | top>`.
pub fn ztrigger(args: &[String]) -> Result<ExitCode> {
    match args.first().map(String::as_str) {
        None | Some("list") => list(),
        Some("rm") | Some("unset") | Some("remove") => rm(args.get(1).map(String::as_str)),
        Some("test") | Some("run") => test(args.get(1).map(String::as_str)),
        Some("tail") | Some("log") => tail(),
        Some("top") | Some("watch") | Some("mon") => top(),
        _ => set(args),
    }
}

/// `git ztrigger DIR CMD... [--throttle <dur>]` — run `CMD` on any file change.
fn set(args: &[String]) -> Result<ExitCode> {
    if args.len() < 2 {
        bail!("usage: git ztrigger <DIR> <command>... [--throttle <dur>]");
    }
    let dir = resolve_dir(&args[0])?;
    // Default throttle collapses the burst of events one file action emits.
    let mut throttle_ms: i64 = 500;
    let mut cmd_parts: Vec<&str> = Vec::new();
    let rest = &args[1..];
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--throttle" | "-t" => {
                let v = rest.get(i + 1).ok_or_else(|| anyhow!("--throttle needs a duration"))?;
                throttle_ms = parse_throttle(v)?;
                i += 2;
            }
            other => {
                cmd_parts.push(other);
                i += 1;
            }
        }
    }
    let cmd = cmd_parts.join(" ");
    if cmd.trim().is_empty() {
        bail!("usage: git ztrigger <DIR> <command>... [--throttle <dur>]");
    }
    let conn = crate::db::open_rw()?;
    crate::db::set_trigger(&conn, &dir, &cmd, throttle_ms)?;
    crate::superset::hooks::reload_daemon();
    let thr = if throttle_ms > 0 {
        format!(" (throttle {throttle_ms}ms)")
    } else {
        " (no throttle)".to_string()
    };
    println!("trigger set: {} -> {cmd}{thr}", dir.display());
    Ok(ExitCode::SUCCESS)
}

/// `git ztrigger list` — every trigger: `path <tab> command <tab> throttle`.
fn list() -> Result<ExitCode> {
    let Ok(conn) = crate::db::open_ro() else {
        return Ok(ExitCode::SUCCESS);
    };
    for t in crate::db::list_triggers(&conn)? {
        let thr = if t.throttle_ms > 0 { format!("{}ms", t.throttle_ms) } else { "-".to_string() };
        println!("{}\t{}\t{}", t.path, t.command, thr);
    }
    Ok(ExitCode::SUCCESS)
}

/// `git ztrigger rm DIR` — remove DIR's trigger.
fn rm(dir: Option<&str>) -> Result<ExitCode> {
    let dir = resolve_dir(dir.unwrap_or("."))?;
    let conn = crate::db::open_rw()?;
    let n = crate::db::remove_trigger(&conn, &dir)?;
    crate::superset::hooks::reload_daemon();
    if n > 0 {
        println!("trigger removed: {}", dir.display());
    } else {
        eprintln!("no trigger set for {}", dir.display());
    }
    Ok(ExitCode::SUCCESS)
}

/// `git ztrigger test DIR` — run DIR's trigger command once now.
fn test(dir: Option<&str>) -> Result<ExitCode> {
    let dir = resolve_dir(dir.unwrap_or("."))?;
    let key = dir.to_string_lossy().into_owned();
    let conn = crate::db::open_ro()?;
    let cmd = crate::db::list_triggers(&conn)?.into_iter().find(|t| t.path == key).map(|t| t.command);
    let Some(cmd) = cmd else {
        bail!("no trigger set for {} (add one with `git ztrigger {} <command>`)", dir.display(), dir.display());
    };
    crate::superset::hooks::run_command(&dir, &cmd);
    Ok(ExitCode::SUCCESS)
}

/// `git zwatch <DIR | list | rm DIR>` — watch DIR and log each change (a trigger
/// with a built-in logging command). `list`/`rm` share the trigger table.
pub fn zwatch(args: &[String]) -> Result<ExitCode> {
    match args.first().map(String::as_str) {
        None => bail!("usage: git zwatch <DIR> | git zwatch <list|rm DIR>"),
        Some("list") => list(),
        Some("rm") | Some("remove") => rm(args.get(1).map(String::as_str)),
        _ => {
            let a = [args[0].clone(), "echo \"[zwatch] $ZVCS_DIR changed\"".to_string()];
            set(&a)
        }
    }
}

// ---- fire recording + live views ------------------------------------------

/// `~/.zvcs/fires.log` — the append-only record the daemon writes on each fire
/// and the `tail`/`top` views read.
fn fires_log() -> PathBuf {
    crate::superset::zdaemon::zvcs_home().join("fires.log")
}

fn epoch_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Record one trigger fire (called by the daemon). `coalesced` is how many events
/// the throttle merged into this fire. Best-effort; keeps the log bounded.
pub fn record_fire(dir: &Path, ok: bool, coalesced: u32) {
    let line = format!("{}\t{}\t{}\t{}\n", epoch_ms(), i32::from(ok), coalesced, dir.display());
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(fires_log()) {
        let _ = f.write_all(line.as_bytes());
        // Bound growth: when the log passes ~1MB, keep only its last half.
        if f.metadata().map(|m| m.len()).unwrap_or(0) > 1_048_576 {
            if let Ok(data) = std::fs::read(fires_log()) {
                let keep = &data[data.len() / 2..];
                let tail: &[u8] = keep.iter().position(|&b| b == b'\n').map(|i| &keep[i + 1..]).unwrap_or(keep);
                let _ = std::fs::write(fires_log(), tail);
            }
        }
    }
}

/// One parsed fire record: `(epoch_ms, ok, coalesced, path)`.
fn parse_fire(line: &str) -> Option<(i64, bool, u32, String)> {
    let mut it = line.splitn(4, '\t');
    let ms = it.next()?.parse().ok()?;
    let ok = it.next()? == "1";
    let coalesced = it.next()?.parse().ok()?;
    let path = it.next()?.to_string();
    Some((ms, ok, coalesced, path))
}

/// Read the recent tail of the fires log (last ~256 KiB) as parsed records.
fn read_fires() -> Vec<(i64, bool, u32, String)> {
    let Ok(data) = std::fs::read(fires_log()) else {
        return Vec::new();
    };
    let start = data.len().saturating_sub(256 * 1024);
    String::from_utf8_lossy(&data[start..]).lines().filter_map(parse_fire).collect()
}

/// `git ztrigger tail` — live stream of fires as they happen.
fn tail() -> Result<ExitCode> {
    let path = fires_log();
    let mut pos = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    eprintln!("{DIM}watching for trigger fires — ctrl-c to stop{RESET}");
    loop {
        if let Ok(mut f) = std::fs::File::open(&path) {
            let len = f.metadata().map(|m| m.len()).unwrap_or(pos);
            if len < pos {
                pos = 0; // log rotated/truncated
            }
            if len > pos {
                f.seek(SeekFrom::Start(pos))?;
                let mut buf = String::new();
                if f.read_to_string(&mut buf).is_ok() {
                    pos += buf.len() as u64;
                    for line in buf.lines() {
                        if let Some((_, ok, coalesced, p)) = parse_fire(line) {
                            let status = if ok { format!("{GREEN}ok{RESET}") } else { format!("{RED}FAIL{RESET}") };
                            let n = coalesced + 1;
                            let events = if n > 1 { format!("  {DIM}({n} events){RESET}") } else { String::new() };
                            println!("{GREEN}●{RESET} {CYAN}{p}{RESET}  {status}{events}");
                        }
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// `git ztrigger top` — an in-place HUD: per-trigger fire count, rate, last-fired.
fn top() -> Result<ExitCode> {
    loop {
        let fires = read_fires();
        let now_ms = epoch_ms();
        let now_s = now_ms / 1000;

        // Aggregate per path: total fires, total events, fires in the last 10s.
        let mut order: Vec<String> = Vec::new();
        let mut agg: std::collections::HashMap<String, (u64, u64, u64, i64)> = std::collections::HashMap::new();
        for (ms, _ok, coalesced, path) in &fires {
            let e = agg.entry(path.clone()).or_insert_with(|| {
                order.push(path.clone());
                (0, 0, 0, 0)
            });
            e.0 += 1;
            e.1 += u64::from(*coalesced) + 1;
            if now_ms - ms <= 10_000 {
                e.2 += 1;
            }
            e.3 = e.3.max(*ms);
        }
        order.sort_by_key(|p| std::cmp::Reverse(agg[p].2)); // hottest first

        print!("\x1b[2J\x1b[H"); // clear + home
        println!("{BOLD}zvcs trigger monitor{RESET}   {DIM}{} trigger(s) firing · ctrl-c to exit{RESET}", order.len());
        println!("{BOLD}{:<42} {:>7} {:>7} {:>8}  {}{RESET}", "DIR", "FIRES", "EV", "/SEC", "LAST");
        if order.is_empty() {
            println!("{DIM}(no fires yet — create a file in a watched dir){RESET}");
        }
        for p in &order {
            let (fires_n, events_n, recent, last_ms) = agg[p];
            let rate = recent as f64 / 10.0;
            let rate_col = if rate >= 3.0 { RED } else if rate >= 1.0 { YELLOW } else { GREEN };
            let last = crate::date::show_date_relative(last_ms / 1000, now_s);
            let short = shorten(p, 42);
            println!(
                "{CYAN}{:<42}{RESET} {:>7} {:>7} {rate_col}{:>7.1}{RESET}  {DIM}{}{RESET}",
                short, fires_n, events_n, rate, last
            );
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Truncate a path to `width` with a leading `…`, replacing `$HOME` with `~`.
fn shorten(path: &str, width: usize) -> String {
    let p = match std::env::var("HOME") {
        Ok(h) if path.starts_with(&h) => format!("~{}", &path[h.len()..]),
        _ => path.to_string(),
    };
    if p.chars().count() <= width {
        p
    } else {
        let tail: String = p.chars().rev().take(width - 1).collect::<Vec<_>>().into_iter().rev().collect();
        format!("…{tail}")
    }
}
