//! `reflog delete` and `reflog expire` — the reflog write path.
//!
//! `delete` drops one entry named by `<ref>@{<n>}` and leaves the rest of the file as
//! it was: the neighbours keep the ids they recorded, and the ref does not move.
//! `expire` drops every entry older than its cutoff, leaving the (now empty) file in
//! place. A selector past the end of a log is silently ignored, as git's is.
//!
//! Expectations measured against stock git 2.55.0.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

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
    /// Three commits on `main` plus a branch checkout, so `logs/HEAD` has six entries
    /// and `logs/refs/heads/main` three.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-reflogw-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        for n in ["1", "2", "3"] {
            f.write("f.txt", n);
            f.git(&["add", "f.txt"]);
            f.git(&["commit", "-q", "-m", &format!("c{n}")]);
        }
        f.git(&["checkout", "-q", "-b", "feature"]);
        f.write("g.txt", "g");
        f.git(&["add", "g.txt"]);
        f.git(&["commit", "-q", "-m", "feature commit"]);
        f.git(&["checkout", "-q", "main"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        self.cmd(args).output().unwrap()
    }

    fn write(&self, path: &str, body: &str) {
        std::fs::write(self.work.join(path), format!("{body}\n")).unwrap();
    }

    fn log_lines(&self, path: &str) -> Vec<String> {
        std::fs::read_to_string(self.work.join(".git/logs").join(path))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

/// `delete` removes exactly the named entry, counted from the newest.
#[test]
fn delete_drops_one_entry_without_touching_the_rest() {
    let f = Fixture::new("delete");
    let before = f.log_lines("HEAD");
    assert_eq!(before.len(), 6, "{before:#?}");
    // `HEAD@{1}` is the second-newest, i.e. the second-to-last line of the file.
    let dropped = before[before.len() - 2].clone();

    let out = f.run(&["reflog", "delete", "HEAD@{1}"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let after = f.log_lines("HEAD");
    assert_eq!(after.len(), 5);
    assert!(!after.contains(&dropped), "the named entry survived");
    // Every other line is byte-identical: no chain rewrite without `--rewrite`.
    let kept: Vec<String> = before.iter().filter(|l| **l != dropped).cloned().collect();
    assert_eq!(after, kept);

    // A selector past the end of the log changes nothing and still exits 0.
    let out = f.run(&["reflog", "delete", "HEAD@{99}"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(f.log_lines("HEAD").len(), 5);
}

/// A branch's own log is addressable, and deleting from it leaves the ref alone.
#[test]
fn delete_targets_the_named_refs_log() {
    let f = Fixture::new("branchlog");
    let head_before = f.log_lines("HEAD");
    let tip_before = String::from_utf8_lossy(&f.run(&["rev-parse", "main"]).stdout)
        .trim_end()
        .to_owned();

    let out = f.run(&["reflog", "delete", "refs/heads/main@{0}"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(f.log_lines("refs/heads/main").len(), 2, "one entry went");
    assert_eq!(f.log_lines("HEAD"), head_before, "HEAD's log is untouched");
    assert_eq!(
        String::from_utf8_lossy(&f.run(&["rev-parse", "main"]).stdout).trim_end(),
        tip_before,
        "the ref does not move without --updateref"
    );
}

/// `expire --all --expire=now` empties every log, leaving the files in place.
#[test]
fn expire_now_empties_every_log() {
    let f = Fixture::new("expire");
    let dry = f.run(&["reflog", "expire", "--dry-run", "--all", "--expire=now"]);
    assert_eq!(dry.status.code(), Some(0), "{dry:?}");
    assert_eq!(f.log_lines("HEAD").len(), 6, "--dry-run writes nothing");

    let out = f.run(&["reflog", "expire", "--all", "--expire=now"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    for log in ["HEAD", "refs/heads/main", "refs/heads/feature"] {
        assert!(f.log_lines(log).is_empty(), "{log} still has entries");
        assert!(
            f.work.join(".git/logs").join(log).exists(),
            "{log} was removed rather than emptied"
        );
    }
}

/// `--expire-unreachable` only reaches the entries whose tip is gone, so a log whose
/// every entry is still reachable survives it untouched.
#[test]
fn expire_unreachable_keeps_reachable_entries() {
    let f = Fixture::new("unreachable");
    let before = f.log_lines("refs/heads/main");
    let out = f.run(&[
        "reflog",
        "expire",
        "--expire-unreachable=now",
        "refs/heads/main",
    ]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(f.log_lines("refs/heads/main"), before);
}
