//! `git zcommands` — a live feed of every git command run across the fleet.
//!
//! Because the zvcs `git` binary shadows real git on PATH, *every* `git <args>`
//! invocation on the machine flows through [`crate::dispatch::run`], which calls
//! [`log_invocation`] at its entry. When command logging is enabled (a marker
//! file, so the disabled path is a single `stat`), each invocation appends one
//! tab-separated line — `ts\tpid\tcwd\targv` — to `$ZVCS_HOME/commands.log`,
//! atomically (O_APPEND, one small write). `git zcommands` enables logging, then
//! prints a backlog and follows the file live, so you watch commands stream in
//! from every repo and every concurrent shell in real time.
//!
//! Usage:
//!   git zcommands                 enable logging, show backlog, follow live
//!   git zcommands -n 100          show the last 100 before following
//!   git zcommands --no-follow     print the backlog and exit
//!   git zcommands --repo zpwr     only commands whose cwd contains "zpwr"
//!   git zcommands --json          NDJSON, one command per line
//!   git zcommands --off           stop logging (remove the marker)
//!   git zcommands --clear         truncate the log

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::Result;

const RESET: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[32m";
const BOLD: &str = "\x1b[1m";

/// How often the live follow polls the log for new lines.
const POLL: Duration = Duration::from_millis(300);

/// Trim the log once it grows past this, keeping the most recent lines, so
/// always-on logging can't grow without bound.
const CAP_BYTES: u64 = 5 * 1024 * 1024;
const KEEP_LINES: usize = 4000;

/// Verbs excluded from the feed — the live viewers themselves, so watching does
/// not spam the feed with its own polling.
fn skip(sub: &str) -> bool {
    matches!(sub, "zcommands" | "zevents" | "ztail" | "ztop")
}

fn log_path() -> PathBuf {
    crate::superset::zdaemon::zvcs_home().join("commands.log")
}

fn marker_path() -> PathBuf {
    crate::superset::zdaemon::zvcs_home().join("commands.enabled")
}

/// Whether command logging is on — a single `stat`, the entire cost on the
/// hot dispatch path when logging is disabled.
pub fn enabled() -> bool {
    marker_path().exists()
}

/// Append one command invocation to the log. Called from [`crate::dispatch::run`]
/// for every command; a no-op (one `stat`) unless logging is enabled. Best-effort
/// — any error is swallowed so logging can never fail a real command.
pub fn log_invocation(sub: &str, args: &[String]) {
    if skip(sub) || !enabled() {
        return;
    }
    let ts = crate::date::now_seconds();
    let pid = std::process::id();
    // ppid identifies the invoking process — which agent / shell ran this git.
    let ppid = unsafe { libc::getppid() };
    let cwd = std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default();
    // argv, space-joined; tabs (our field separator) can't appear in a verb/args
    // git would accept, but strip any defensively.
    let mut argv = sub.to_string();
    for a in args {
        argv.push(' ');
        argv.push_str(a);
    }
    let argv = argv.replace(['\t', '\n'], " ");
    let cwd = cwd.replace('\t', " ");
    let line = format!("{ts}\t{pid}\t{ppid}\t{cwd}\t{argv}\n");

    let path = log_path();
    // The append and the trim share the log's lock. An `O_APPEND` write lands
    // whole on its own, but `trim` reads the file and writes back what it kept,
    // so a line appended between those two steps is erased by a rewrite that
    // never saw it. Measured: sixteen commands run at once across a trim, three
    // of their entries gone from the audit trail, every command exit 0.
    //
    // The cost is one `flock` pair per command, and only while logging is on:
    // the hot path when it is off is still the single failed `stat` above.
    let _lock = crate::superset::registry::lock("commands");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = f.write_all(line.as_bytes());
    }
    // Occasionally trim if oversized (cheap size check; rewrite only when over).
    if let Ok(m) = std::fs::metadata(&path) {
        if m.len() > CAP_BYTES {
            trim(&path);
        }
    }
}

/// Rewrite the log keeping only the most recent `KEEP_LINES` lines.
fn trim(path: &Path) {
    let Ok(data) = std::fs::read(path) else { return };
    let text = String::from_utf8_lossy(&data);
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(KEEP_LINES);
    let kept = lines[start..].join("\n");
    let _ = std::fs::write(path, format!("{kept}\n"));
}

struct Opts {
    limit: usize,
    follow: bool,
    json: bool,
    repo: Option<String>,
}

fn parse(args: &[String]) -> Result<Option<Opts>> {
    let mut o = Opts { limit: 30, follow: true, json: false, repo: None };
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
            "--repo" | "-r" => o.repo = it.next().cloned(),
            "--off" | "--stop" => {
                let _ = std::fs::remove_file(marker_path());
                eprintln!("zvcs: command logging disabled");
                return Ok(None);
            }
            "--clear" => {
                let _ = std::fs::write(log_path(), b"");
                eprintln!("zvcs: command log cleared");
                return Ok(None);
            }
            _ => {}
        }
    }
    Ok(Some(o))
}

pub fn zcommands(args: &[String]) -> Result<ExitCode> {
    let Some(o) = parse(args)? else { return Ok(ExitCode::SUCCESS) };

    // Enable logging (idempotent). Note it the first time so the user knows why
    // the feed may be empty until commands start running.
    if !enabled() {
        let _ = std::fs::create_dir_all(crate::superset::zdaemon::zvcs_home());
        if std::fs::write(marker_path(), b"").is_ok() && !o.json {
            eprintln!(
                "{DIM}command logging enabled — every `git` across the fleet now streams here \
                 ({}); `git zcommands --off` to stop{RESET}",
                log_path().display()
            );
        }
    }

    let path = log_path();

    // Backlog: the last `limit` matching lines, oldest-first.
    let backlog = read_backlog(&path, o.limit, o.repo.as_deref());
    for rec in &backlog {
        emit(rec, o.json);
    }
    let mut offset = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);

    if !o.follow {
        return Ok(ExitCode::SUCCESS);
    }
    if !o.json {
        eprintln!("{DIM}live — every git command across the fleet — ctrl-c to stop{RESET}");
    }
    loop {
        let len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if len < offset {
            offset = 0; // truncated / rotated
        }
        if len > offset {
            offset = follow_from(&path, offset, &o);
        }
        std::thread::sleep(POLL);
    }
}

/// Read new complete lines from `offset` to EOF, emit the matching ones, and
/// return the new offset (advanced only past complete `\n`-terminated lines, so a
/// half-written trailing line is re-read next poll).
fn follow_from(path: &Path, offset: u64, o: &Opts) -> u64 {
    let Ok(mut f) = std::fs::File::open(path) else { return offset };
    if f.seek(SeekFrom::Start(offset)).is_err() {
        return offset;
    }
    let mut buf = Vec::new();
    if f.read_to_end(&mut buf).is_err() {
        return offset;
    }
    let text = String::from_utf8_lossy(&buf);
    let mut consumed = 0usize;
    for line in text.split_inclusive('\n') {
        if !line.ends_with('\n') {
            break; // partial trailing line — leave it for the next poll
        }
        consumed += line.len();
        if let Some(rec) = parse_line(line.trim_end_matches('\n')) {
            if pass(&rec, o) {
                emit(&rec, o.json);
            }
        }
    }
    offset + consumed as u64
}

/// The last `limit` matching records from the log, oldest-first.
fn read_backlog(path: &Path, limit: usize, repo: Option<&str>) -> Vec<Rec> {
    let Ok(data) = std::fs::read(path) else { return Vec::new() };
    let text = String::from_utf8_lossy(&data);
    let mut out: Vec<Rec> = text
        .lines()
        .filter_map(parse_line)
        .filter(|r| repo.map(|s| r.cwd.contains(s)).unwrap_or(true))
        .collect();
    let start = out.len().saturating_sub(limit);
    out.drain(..start);
    out
}

/// One logged command.
struct Rec {
    ts: i64,
    pid: u32,
    ppid: i32,
    cwd: String,
    argv: String,
}

fn parse_line(line: &str) -> Option<Rec> {
    let mut it = line.splitn(5, '\t');
    let ts = it.next()?.parse().ok()?;
    let pid = it.next()?.parse().ok()?;
    let ppid = it.next()?.parse().ok()?;
    let cwd = it.next()?.to_string();
    let argv = it.next()?.to_string();
    Some(Rec { ts, pid, ppid, cwd, argv })
}

fn pass(r: &Rec, o: &Opts) -> bool {
    o.repo.as_deref().map(|s| r.cwd.contains(s)).unwrap_or(true)
}

/// Print one record — a colored human line, or one NDJSON object with `--json`.
fn emit(r: &Rec, json: bool) {
    if json {
        let obj = serde_json::json!({
            "ts": r.ts,
            "pid": r.pid,
            "ppid": r.ppid,
            "cwd": r.cwd,
            "argv": r.argv,
        });
        println!("{obj}");
        return;
    }
    // pid←ppid: the git process and the agent/shell that launched it.
    println!(
        "{DIM}{} {:>7}\u{2190}{:<7}{RESET}  {CYAN}{:<20}{RESET}  {GREEN}git{RESET} {BOLD}{}{RESET}",
        hms(r.ts),
        r.pid,
        r.ppid,
        basename(&r.cwd),
        r.argv,
    );
}

/// The final path component of `cwd`, for a compact repo label.
fn basename(cwd: &str) -> String {
    Path::new(cwd)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| cwd.to_string())
}

/// Local-time `HH:MM:SS` for epoch seconds (via libc, no tz crate) — as `zevents`.
fn hms(ts: i64) -> String {
    let t = ts as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return "--:--:--".to_string();
    }
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_roundtrips_through_parse() {
        let r = parse_line("1700000000\t4242\t900\t/home/u/repo\tfetch --all").unwrap();
        assert_eq!(r.ts, 1700000000);
        assert_eq!(r.pid, 4242);
        assert_eq!(r.ppid, 900);
        assert_eq!(r.cwd, "/home/u/repo");
        assert_eq!(r.argv, "fetch --all");
        // argv keeps embedded spaces; only the first 4 tabs split fields.
        let r2 = parse_line("1\t2\t3\t/p\tcommit -m msg with spaces").unwrap();
        assert_eq!(r2.argv, "commit -m msg with spaces");
        assert!(parse_line("garbage").is_none());
    }

    #[test]
    fn backlog_filters_by_repo_and_limits() {
        let dir = std::env::temp_dir().join(format!("zvcs-cmdlog-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("commands.log");
        let body = "\
1\t10\t99\t/x/alpha\tstatus
2\t11\t99\t/x/beta\tfetch
3\t12\t99\t/x/alpha\tcommit -am z
4\t13\t99\t/x/beta\tpush\n";
        std::fs::write(&path, body).unwrap();
        let all = read_backlog(&path, 100, None);
        assert_eq!(all.len(), 4);
        let alpha = read_backlog(&path, 100, Some("alpha"));
        assert_eq!(alpha.len(), 2);
        assert_eq!(alpha[0].argv, "status");
        let last1 = read_backlog(&path, 1, None);
        assert_eq!(last1.len(), 1);
        assert_eq!(last1[0].argv, "push");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn viewers_are_skipped() {
        assert!(skip("zcommands"));
        assert!(skip("ztop"));
        assert!(!skip("fetch"));
        assert!(!skip("zforeach"));
    }
}
