//! The reflog a rebase leaves on `HEAD`.
//!
//! `sequencer_reflog_action()` prefixes every entry with `GIT_REFLOG_ACTION`
//! when the caller set one — `pull` sets it to its own command line — and with
//! `rebase` otherwise. The run ends with `(finish): returning to <ref>`, which
//! is what tells `git reflog` where the rebase put you; the vendored `gix-ref`
//! drops reflog lines for symbolic-target updates, so that entry has to be
//! written explicitly.
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
    /// `topic` with one commit, forked from a `main` that has moved on.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-rbreflog-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        std::fs::write(f.work.join("a.txt"), "base\n").unwrap();
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "base"]);
        f.git(&["checkout", "-q", "-b", "topic"]);
        std::fs::write(f.work.join("t.txt"), "t\n").unwrap();
        f.git(&["add", "t.txt"]);
        f.git(&["commit", "-q", "-m", "topicwork"]);
        f.git(&["checkout", "-q", "main"]);
        std::fs::write(f.work.join("m.txt"), "m\n").unwrap();
        f.git(&["add", "m.txt"]);
        f.git(&["commit", "-q", "-m", "mainwork"]);
        f.git(&["checkout", "-q", "topic"]);
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

    fn run(&self, args: &[&str]) -> (bool, String, String) {
        let out = self.cmd(args).output().unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// The reflog messages on `HEAD`, newest first.
    fn head_reflog(&self) -> Vec<String> {
        let out = self.cmd(&["reflog", "show", "HEAD", "--format=%gs"]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).lines().map(str::to_owned).collect()
    }
}

/// A plain rebase: `rebase (start)`, one `(pick)` per replayed commit, and the
/// `(finish)` that re-attaches `HEAD`.
#[test]
fn a_rebase_records_start_pick_and_finish() {
    let f = Fixture::new("plain");
    let (ok, out, err) = f.run(&["rebase", "main"]);
    assert!(ok, "rebase failed: {out}{err}");

    let log = f.head_reflog();
    assert_eq!(
        &log[..3],
        [
            "rebase (finish): returning to refs/heads/topic",
            "rebase (pick): topicwork",
            "rebase (start): checkout main",
        ],
        "reflog: {log:?}"
    );
}

/// The caller's `GIT_REFLOG_ACTION` replaces the `rebase` prefix, which is how a
/// `pull --rebase` is distinguishable from a hand-run rebase afterwards.
#[test]
fn git_reflog_action_replaces_the_prefix() {
    let f = Fixture::new("action");
    let out = f
        .cmd(&["rebase", "main"])
        .env("GIT_REFLOG_ACTION", "pull --rebase")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "rebase failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let log = f.head_reflog();
    assert_eq!(
        &log[..3],
        [
            "pull --rebase (finish): returning to refs/heads/topic",
            "pull --rebase (pick): topicwork",
            "pull --rebase (start): checkout main",
        ],
        "reflog: {log:?}"
    );
}
