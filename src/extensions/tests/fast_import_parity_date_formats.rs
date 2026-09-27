//! `fast-import --date-format=rfc2822` and `--date-format=now`.
//!
//! `parse_ident()` (builtin/fast-import.c:2026-2044) keeps the ident through
//! the space after `>` and then, for `rfc2822`, appends `parse_date()`'s
//! `<seconds> <±hhmm>` (date.c:979-987) or dies `invalid rfc2822 date "<d>" in
//! ident: <ident>`; for `now` it insists the date is the literal `now`
//! (`date in ident must be 'now': <ident>`) and appends `datestamp()`
//! (date.c:1057-1069). zvcs accepted both names and then refused every ident.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.
//! Stock also prints `fast-import: dumping crash report to …` after a fatal,
//! which this port does not write (see the module header of fast_import.rs),
//! so the fatal cases check the `fatal:` line and the exit code.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    work: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-fast-import-dates-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "."], "");
        f
    }

    fn run(&self, args: &[&str], stdin: &str) -> (String, String, i32) {
        let mut child = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn commit_body(&self, rev: &str) -> String {
        self.run(&["cat-file", "commit", rev], "").0
    }
}

const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

#[test]
fn rfc2822_dates_are_stored_as_seconds_and_offset() {
    let f = Fixture::new("rfc");
    let stream = "commit refs/heads/x\n\
                  committer A <a@x> Tue, 14 Nov 2023 22:13:20 +0100\n\
                  data 0\n\n";
    let (out, err, code) = f.run(&["fast-import", "--quiet", "--date-format=rfc2822"], stream);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "", 0));
    assert_eq!(
        f.commit_body("x"),
        format!(
            "tree {EMPTY_TREE}\n\
             author A <a@x> 1699996400 +0100\n\
             committer A <a@x> 1699996400 +0100\n\n"
        )
    );
}

#[test]
fn the_feature_command_selects_rfc2822_for_every_ident() {
    let f = Fixture::new("feature");
    let stream = "feature date-format=rfc2822\n\
                  commit refs/heads/y\n\
                  author B <b@x> 2023-11-14 10:00:00 -0530\n\
                  committer A <a@x> Tue, 14 Nov 2023 22:13:20 +0000\n\
                  data 0\n\n";
    let (out, err, code) = f.run(&["fast-import", "--quiet"], stream);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "", 0));
    assert_eq!(
        f.commit_body("y"),
        format!(
            "tree {EMPTY_TREE}\n\
             author B <b@x> 1699975800 -0530\n\
             committer A <a@x> 1700000000 +0000\n\n"
        )
    );
}

#[test]
fn an_unreadable_rfc2822_date_is_fatal() {
    let f = Fixture::new("garbage");
    let stream = "commit refs/heads/z\ncommitter A <a@x> garbage\ndata 0\n\n";
    let (out, err, code) = f.run(&["fast-import", "--quiet", "--date-format=rfc2822"], stream);
    assert_eq!((out.as_str(), code), ("", 128));
    assert_eq!(
        err.lines().next(),
        Some("fatal: invalid rfc2822 date \"garbage\" in ident: A <a@x> garbage")
    );
    assert_eq!(f.run(&["rev-parse", "--verify", "-q", "refs/heads/z"], "").2, 1);
}

#[test]
fn now_takes_the_literal_now_and_stamps_the_wall_clock() {
    let f = Fixture::new("now");
    let stream = "commit refs/heads/w\ncommitter A <a@x> 1700000000 +0000\ndata 0\n\n";
    let (out, err, code) = f.run(&["fast-import", "--quiet", "--date-format=now"], stream);
    assert_eq!((out.as_str(), code), ("", 128));
    assert_eq!(
        err.lines().next(),
        Some("fatal: date in ident must be 'now': A <a@x> 1700000000 +0000")
    );

    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let stream = "commit refs/heads/w\ncommitter A <a@x> now\ndata 0\n\n";
    let (out, err, code) = f.run(&["fast-import", "--quiet", "--date-format=now"], stream);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "", 0));
    let body = f.commit_body("w");
    let committer = body.lines().find_map(|l| l.strip_prefix("committer A <a@x> ")).unwrap();
    // TZ=UTC for the child, so `datestamp()` stamps +0000.
    let (seconds, zone) = committer.split_once(' ').unwrap();
    let seconds: u64 = seconds.parse().unwrap();
    assert!(seconds >= before && seconds <= before + 60, "{committer}");
    assert_eq!(zone, "+0000");
}
