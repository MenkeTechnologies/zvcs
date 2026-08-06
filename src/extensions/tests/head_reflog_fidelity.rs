//! What lands in `.git/logs/HEAD` when `HEAD` moves.
//!
//! Every mover writes the entry `reset_head()`/`update_ref()` would: the *previous*
//! `HEAD` id in the old field (not the null id, which is what a symbolic previous
//! value yields if the log line is left as the ref layer wrote it), and the message
//! the command chose — `checkout: moving from <a> to <b>` for a checkout, and
//! `<reflog action>: checkout <spec>` for the checkouts a rebase performs.
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
    /// `main` with two commits, plus a `side` branch at the tip.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-headreflog-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        f.write("f.txt", b"a\n");
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "one"]);
        f.write("f.txt", b"b\n");
        f.git(&["commit", "-q", "-am", "two"]);
        f.git(&["branch", "side"]);
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
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn write(&self, path: &str, body: &[u8]) {
        std::fs::write(self.work.join(path), body).unwrap();
    }

    fn rev(&self, spec: &str) -> String {
        let out = self.cmd(&["rev-parse", spec]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// `(old id, new id, message)` of the last `.git/logs/HEAD` entry.
    fn last_head_log(&self) -> (String, String, String) {
        let text = std::fs::read_to_string(self.work.join(".git/logs/HEAD")).unwrap();
        let line = text.lines().last().expect("a HEAD reflog entry").to_string();
        let (ids, message) = line.split_once('\t').expect("tab before the message");
        let mut fields = ids.split(' ');
        (
            fields.next().unwrap().to_string(),
            fields.next().unwrap().to_string(),
            message.to_string(),
        )
    }
}

const NULL_ID: &str = "0000000000000000000000000000000000000000";

#[test]
fn switching_to_a_branch_records_both_ends() {
    let f = Fixture::new("attach");
    let before = f.rev("HEAD");
    f.git(&["switch", "side"]);
    let (old, new, message) = f.last_head_log();
    assert_eq!(old, before);
    assert_eq!(new, f.rev("side"));
    assert_eq!(message, "checkout: moving from main to side");
}

#[test]
fn detaching_records_the_spelling_the_caller_used() {
    let f = Fixture::new("detach");
    let before = f.rev("HEAD");
    f.git(&["switch", "--detach", "HEAD~1"]);
    let (old, new, message) = f.last_head_log();
    assert_eq!(old, before, "the old field must not be the null id");
    assert_ne!(old, NULL_ID);
    assert_eq!(new, f.rev("HEAD"));
    assert_eq!(message, "checkout: moving from main to HEAD~1");

    // A bare `--detach` detaches where `HEAD` already is, and git logs it as `HEAD`.
    let f = Fixture::new("detach-bare");
    f.git(&["switch", "--detach"]);
    assert_eq!(f.last_head_log().2, "checkout: moving from main to HEAD");
}

#[test]
fn creating_a_branch_records_the_start_point_as_given() {
    let f = Fixture::new("create");
    f.git(&["switch", "-c", "fresh"]);
    let log = std::fs::read_to_string(f.work.join(".git/logs/refs/heads/fresh")).unwrap();
    assert!(log.contains("branch: Created from HEAD\n"), "{log}");

    let f = Fixture::new("create-from");
    f.git(&["switch", "-c", "other", "side"]);
    let log = std::fs::read_to_string(f.work.join(".git/logs/refs/heads/other")).unwrap();
    assert!(log.contains("branch: Created from side\n"), "{log}");
}

/// A rebase names the commit it detaches at the way the caller spelled it, and keeps
/// the previous `HEAD` in the old field.
#[test]
fn rebase_records_the_onto_spelling() {
    let f = Fixture::new("rebase");
    let before = f.rev("HEAD");
    f.git(&["rebase", "-i", "HEAD~1"]);

    let text = std::fs::read_to_string(f.work.join(".git/logs/HEAD")).unwrap();
    let start = text
        .lines()
        .find(|l| l.contains("(start): checkout"))
        .expect("a rebase start entry");
    assert!(start.ends_with("rebase (start): checkout HEAD~1"), "{start}");
    assert!(start.starts_with(&before), "old field must be the previous HEAD: {start}");
}

/// Checking out the `<branch>` argument is reflogged as the rebase's own action.
#[test]
fn rebase_of_another_branch_logs_its_checkout() {
    let f = Fixture::new("rebase-branch");
    // Give `side` a commit of its own so the rebase has something to do.
    f.git(&["switch", "-q", "side"]);
    f.write("g.txt", b"g\n");
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "side work"]);
    f.git(&["switch", "-q", "main"]);
    let before = f.rev("HEAD");

    f.git(&["rebase", "main", "side"]);
    let text = std::fs::read_to_string(f.work.join(".git/logs/HEAD")).unwrap();
    let line = text
        .lines()
        .find(|l| l.contains("rebase: checkout side"))
        .expect("the switch-to entry");
    assert!(line.starts_with(&before), "old field must be the previous HEAD: {line}");
    // That entry is the last one: the rebase's checkout is logged as the rebase's own
    // action, not as a plain `checkout: moving from …`.
    assert_eq!(
        text.lines().last().map(str::to_string),
        Some(line.to_string()),
        "{text}"
    );
}
