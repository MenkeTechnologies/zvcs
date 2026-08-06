//! `log --stdin` and `show --stdin`: revisions read from standard input.
//!
//! `read_revisions_from_stdin()` takes another revision argument per line, and a
//! bare `--` turns the rest into pathspecs. It is how a caller asks about a set of
//! commits too large or too dynamic to put on a command line — the JetBrains
//! client loads every commit's details with `log --no-walk --stdin`, feeding the
//! hashes it wants and reading records back. Without it both of its detail panes
//! fail at once while the commit list beside them loads normally, because that
//! list comes from an ordinary `log` and only the details go through stdin.
//!
//! Expectations measured against stock git.
#![cfg(unix)]

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    repo: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-logstdin-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let f = Fixture { root, repo };
        f.git(&["init", "-q", "-b", "master", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        for (name, body, msg) in [
            ("a.txt", "one\n", "first"),
            ("a.txt", "one\ntwo\n", "second"),
            ("b.txt", "b\n", "third"),
        ] {
            std::fs::write(f.repo.join(name), body).unwrap();
            f.git(&["add", "-A"]);
            f.git(&["commit", "-q", "-m", msg]);
        }
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    /// Run with `input` on stdin.
    fn fed(&self, args: &[&str], input: &str) -> std::process::Output {
        let mut child = self
            .cmd(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(input.as_bytes())
            .expect("write stdin");
        child.wait_with_output().expect("wait")
    }

    fn rev(&self, spec: &str) -> String {
        let out = self.cmd(&["rev-parse", spec]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).trim_end().to_owned()
    }
}

/// The client's own detail-loading shape: hashes in, one record each, no walk.
#[test]
fn log_reads_revisions_from_stdin() {
    let f = Fixture::new("log");
    let head = f.rev("HEAD");
    let prev = f.rev("HEAD~1");

    let out = f.fed(
        &["log", "--no-walk", "--stdin", "--format=%H", "--encoding=UTF-8", "--name-status"],
        &format!("{head}\n{prev}\n"),
    );
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    // Both named commits, in the order the hashes were fed, each with its own
    // name-status record and nothing traversed beyond them.
    assert!(text.starts_with(&head), "{text}");
    assert!(text.contains(&prev), "{text}");
    assert!(text.contains("A\tb.txt"), "{text}");
    assert!(text.contains("M\ta.txt"), "{text}");
    assert!(!text.contains(&f.rev("HEAD~2")), "the walk did not stop: {text}");
}

/// Without `--no-walk` the fed revisions are ordinary tips and history is walked.
#[test]
fn stdin_revisions_are_walked_like_arguments() {
    let f = Fixture::new("walk");
    let head = f.rev("HEAD");

    let piped = f.fed(&["log", "--stdin", "--format=%s"], &format!("{head}\n"));
    assert_eq!(piped.status.code(), Some(0), "{piped:?}");
    let named = f.cmd(&["log", "--format=%s", &head]).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&piped.stdout),
        String::from_utf8_lossy(&named.stdout),
        "a revision on stdin walks exactly as one on the command line"
    );
    assert_eq!(String::from_utf8_lossy(&piped.stdout), "third\nsecond\nfirst\n");
}

/// A bare `--` on stdin ends the revisions; the rest are pathspecs.
#[test]
fn a_separator_on_stdin_starts_the_pathspecs() {
    let f = Fixture::new("paths");
    let head = f.rev("HEAD");

    let out = f.fed(&["log", "--stdin", "--format=%s"], &format!("{head}\n--\na.txt\n"));
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    // `b.txt`'s commit does not touch `a.txt`, so only the two that do are shown.
    assert_eq!(String::from_utf8_lossy(&out.stdout), "second\nfirst\n");
}

/// `show` takes the same input.
#[test]
fn show_reads_revisions_from_stdin() {
    let f = Fixture::new("show");
    let head = f.rev("HEAD");
    let prev = f.rev("HEAD~1");

    let out = f.fed(&["show", "--stdin", "-s", "--format=%H"], &format!("{head}\n{prev}\n"));
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), format!("{head}\n{prev}\n"));
}
