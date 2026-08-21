//! Every line in the singleton daemon log (`$ZVCS_HOME/zvcs.log`) must open with
//! a wall-clock stamp. Without one the file is a pile of undated chatter: you
//! cannot tell a crawl that ran a second ago from one that ran last week, cannot
//! correlate a `[zvcs job]` line with a shell command, and `zdaemon log -f`
//! shows lines with no way to spot a stall.
//!
//! Two writers reach that file and both are covered here: the direct writer
//! (`zdaemon::log_line`, exercised through the detached `zreindex` child) and the
//! daemon's own stdout, which IS the log when it runs detached (exercised by the
//! watcher's "watching N path(s)" line).

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// `YYYY-MM-DDTHH:MM:SS±HH:MM ` — RFC-3339 local, the `iso-strict-local` spelling.
fn stamp_re() -> regex::Regex {
    regex::Regex::new(r"^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})(Z|[+-]\d{2}:\d{2}) ")
        .expect("stamp pattern")
}

fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-logstamp-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir.canonicalize().expect("canonicalize temp dir")
}

/// A repo with one commit, so the crawl has something to index.
fn repo_with_a_commit(dir: &Path) {
    std::fs::create_dir_all(dir).expect("create repo dir");
    run(dir, &["init", "-q", "-b", "main"], &[]);
    run(dir, &["commit", "--allow-empty", "-q", "-m", "root"], &[]);
}

fn run(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> std::process::Output {
    let mut cmd = Command::new(BIN);
    cmd.args(["-c", "user.email=test@example.com", "-c", "user.name=zvcs-test"])
        .args(args)
        .current_dir(dir);
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().unwrap_or_else(|e| panic!("git {args:?}: {e}"))
}

fn log_text(home: &Path) -> String {
    std::fs::read_to_string(home.join("zvcs.log")).unwrap_or_default()
}

/// The year/month/day/hour the stamp claims, as a rough epoch-seconds value. Only
/// used to prove the stamp is *now* and not a zeroed clock — a `show_date(0, …)`
/// regression would still match the shape but would read 1970.
fn plausibly_now(caps: &regex::Captures<'_>) -> bool {
    let year: i32 = caps[1].parse().expect("year");
    let this_year = 1900
        + unsafe {
            let now = libc::time(std::ptr::null_mut());
            let mut tm: libc::tm = std::mem::zeroed();
            libc::localtime_r(&now, &mut tm);
            tm.tm_year
        };
    year == this_year
}

#[test]
fn detached_reindex_stamps_the_line_it_writes() {
    let root = tempdir("reindex");
    let home = root.join("home");
    let indexed = root.join("indexed");
    repo_with_a_commit(&indexed);

    // The env marker is what the async form sets on its detached child; with it,
    // the result is a log record rather than a bare stdout line landing in the log.
    let out = run(
        &root,
        &["zreindex", "--sync", indexed.to_str().expect("utf-8 path")],
        &[("ZVCS_HOME", home.to_str().expect("utf-8 path")), ("ZVCS_REINDEX_DETACHED", "1")],
    );
    assert!(out.status.success(), "zreindex failed: {}", String::from_utf8_lossy(&out.stderr));

    let text = log_text(&home);
    let re = stamp_re();
    let mut saw_crawl = false;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let caps = re
            .captures(line)
            .unwrap_or_else(|| panic!("log line has no timestamp: {line:?}"));
        assert!(plausibly_now(&caps), "stamp is not the current year: {line:?}");
        saw_crawl |= line.contains("[zvcs crawl] indexed");
    }
    assert!(saw_crawl, "detached reindex wrote no crawl result to the log:\n{text}");

    // The tag survives the stamp — `grep '\[zvcs crawl\]' zvcs.log` still works.
    assert!(
        text.contains(" [zvcs crawl] indexed 1 repo(s)"),
        "crawl line lost its tag or its count:\n{text}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// The stamping must not leak into `zreindex`'s documented stdout contract:
/// piped/scripted `--sync` still prints a bare `indexed N repo(s), pruned M`.
#[test]
fn plain_sync_reindex_keeps_its_bare_line_on_stdout() {
    let root = tempdir("stdout");
    let home = root.join("home");
    let indexed = root.join("indexed");
    repo_with_a_commit(&indexed);

    let out = run(
        &root,
        &["zreindex", "--sync", indexed.to_str().expect("utf-8 path")],
        &[("ZVCS_HOME", home.to_str().expect("utf-8 path"))],
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        stdout.starts_with("indexed 1 repo(s), pruned "),
        "stdout is no longer the bare result line: {stdout:?}"
    );
    assert!(
        !log_text(&home).contains("[zvcs crawl] indexed"),
        "an inline --sync must not also write the result to the log"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// The daemon's own stdout IS the log when it is detached, so the watcher's
/// chatter has to carry the same stamp — otherwise half the file is dated and
/// half is not.
#[test]
fn watcher_chatter_carries_the_same_stamp() {
    let root = tempdir("watch");
    let home = root.join("home");
    let repo = root.join("repo");
    repo_with_a_commit(&repo);
    // Something for the watcher to arm on.
    run(&repo, &["config", "zvcs.autostatus", "true"], &[]);
    run(&repo, &["config", "zvcs.interval", "1"], &[]);

    let daemon_log = root.join("daemon.log");
    let logf = std::fs::File::create(&daemon_log).expect("create daemon log");
    let mut daemon: Child = Command::new(BIN)
        .args(["zdaemon", "start", "--foreground"])
        .current_dir(&repo)
        .env("ZVCS_HOME", &home)
        .env("ZVCS_SOCK", root.join("zvcs-test.sock"))
        .stdin(Stdio::null())
        .stdout(Stdio::from(logf.try_clone().expect("clone log handle")))
        .stderr(Stdio::from(logf))
        .spawn()
        .expect("spawn zdaemon");

    let deadline = Instant::now() + Duration::from_secs(20);
    let re = stamp_re();
    let mut stamped = None;
    while Instant::now() < deadline && stamped.is_none() {
        let text = std::fs::read_to_string(&daemon_log).unwrap_or_default();
        stamped = text.lines().find(|l| l.contains("[zvcs watch] watching")).map(str::to_owned);
        if stamped.is_none() {
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    let _ = daemon.kill();
    let _ = daemon.wait();

    let line = stamped.expect("watcher never reported its armed watches");
    let caps = re
        .captures(&line)
        .unwrap_or_else(|| panic!("watcher line has no timestamp: {line:?}"));
    assert!(plausibly_now(&caps), "watcher stamp is not the current year: {line:?}");
    let _ = std::fs::remove_dir_all(&root);
}
