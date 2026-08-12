//! The two reflog entries git's files backend synthesises that have no ref edit of
//! their own, and the one it deliberately withholds.
//!
//! `lock_ref_for_update()` (refs/files-backend.c) turns a single caller request into
//! several updates before anything is written:
//!
//! * `split_symref_update()` rewrites an edit of `HEAD` into a real edit of the branch
//!   plus a `REF_LOG_ONLY` edit of `HEAD`;
//! * `split_head_update()` adds a `REF_LOG_ONLY` edit of `HEAD` when the caller edits
//!   the branch `HEAD` points at directly;
//! * and `files_transaction_finish()` writes a reflog entry for an update that is
//!   `REF_NEEDS_COMMIT || REF_LOG_ONLY` — so the log-only halves are logged even when
//!   the value does not move, while the branch itself is not.
//!
//! Each assertion below was measured against stock git 2.55.0.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");
const NULL_ID: &str = "0000000000000000000000000000000000000000";

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
    /// `main` with two commits.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-reflogsplit-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        std::fs::write(f.work.join("f.txt"), b"a\n").unwrap();
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "one"]);
        std::fs::write(f.work.join("f.txt"), b"b\n").unwrap();
        f.git(&["commit", "-q", "-am", "two"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_EDITOR", "true");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn rev(&self, spec: &str) -> String {
        let out = self.cmd(&["rev-parse", spec]).output().unwrap();
        assert!(out.status.success(), "`git rev-parse {spec}` failed: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn log_lines(&self, rel: &str) -> Vec<String> {
        std::fs::read_to_string(self.work.join(".git/logs").join(rel))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn log_exists(&self, rel: &str) -> bool {
        self.work.join(".git/logs").join(rel).exists()
    }
}

/// `(old id, new id, message)`; the message is empty when the entry carries none.
fn split_entry(line: &str) -> (String, String, String) {
    let (ids, message) = match line.split_once('\t') {
        Some((ids, message)) => (ids, message.to_string()),
        None => (line, String::new()),
    };
    let mut fields = ids.split(' ');
    (
        fields.next().unwrap().to_string(),
        fields.next().unwrap().to_string(),
        message,
    )
}

/// `git reset` with no argument moves `HEAD` to where it already is. The branch value
/// does not change, so `REF_NEEDS_COMMIT` stays clear and `refs/heads/main` is not
/// logged — but the log-only `HEAD` half is written regardless.
#[test]
fn reset_to_head_logs_head_only() {
    let f = Fixture::new("reset-noop");
    let head = f.rev("HEAD");
    let branch_before = f.log_lines("refs/heads/main").len();

    std::fs::write(f.work.join("f.txt"), b"dirty\n").unwrap();
    f.git(&["reset"]);

    let (old, new, message) = split_entry(f.log_lines("HEAD").last().expect("a HEAD entry"));
    assert_eq!(old, head, "old and new are both the unmoved HEAD");
    assert_eq!(new, head);
    assert_eq!(message, "reset: moving to HEAD");
    assert_eq!(
        f.log_lines("refs/heads/main").len(),
        branch_before,
        "the branch value never moved, so its log must be untouched"
    );
}

/// Editing the checked-out branch by name is mirrored into `.git/logs/HEAD` with the
/// same ids and message — `split_head_update()`'s whole purpose.
#[test]
fn updating_the_current_branch_mirrors_into_head() {
    let f = Fixture::new("split-head");
    let before = f.rev("HEAD");
    let target = f.rev("HEAD~1");

    f.git(&["update-ref", "-m", "parity update", "refs/heads/main", "HEAD~1"]);

    let head = split_entry(f.log_lines("HEAD").last().expect("a HEAD entry"));
    let branch = split_entry(f.log_lines("refs/heads/main").last().expect("a branch entry"));
    assert_eq!(head, (before.clone(), target.clone(), "parity update".to_string()));
    assert_eq!(branch, (before, target, "parity update".to_string()));
}

/// A branch that `HEAD` does *not* point at gets no mirrored entry.
#[test]
fn updating_another_branch_leaves_head_alone() {
    let f = Fixture::new("split-head-other");
    f.git(&["branch", "side", "HEAD~1"]);
    let head_before = f.log_lines("HEAD");

    f.git(&["update-ref", "refs/heads/side", "HEAD"]);

    assert_eq!(f.log_lines("HEAD"), head_before);
}

/// `--no-deref` writes `HEAD` itself. git still resolves the symref first so the
/// entry's old field is the value `HEAD` pointed at, not the null id.
#[test]
fn no_deref_head_records_the_resolved_previous_value() {
    let f = Fixture::new("no-deref");
    let before = f.rev("HEAD");
    let target = f.rev("HEAD~1");

    f.git(&["update-ref", "--no-deref", "HEAD", "HEAD~1"]);

    let (old, new, _) = split_entry(f.log_lines("HEAD").last().expect("a HEAD entry"));
    assert_ne!(old, NULL_ID, "a symbolic previous value must still resolve");
    assert_eq!(old, before);
    assert_eq!(new, target);
}

/// Deleting through `HEAD` removes the branch and the branch's log, but the log-only
/// `HEAD` half both survives and gains a `<old> <null>` entry.
#[test]
fn deleting_through_head_keeps_the_head_log() {
    let f = Fixture::new("delete-head");
    let before = f.rev("HEAD");

    f.git(&["update-ref", "-d", "HEAD"]);

    assert!(f.log_exists("HEAD"), ".git/logs/HEAD must survive a log-only delete");
    assert!(
        !f.log_exists("refs/heads/main"),
        "the real delete takes the branch log with it"
    );
    let (old, new, message) = split_entry(f.log_lines("HEAD").last().expect("a HEAD entry"));
    assert_eq!(old, before);
    assert_eq!(new, NULL_ID);
    assert_eq!(message, "", "`update-ref -d` passes no message");
    assert!(
        !f.work.join(".git/refs/heads/main").exists(),
        "the branch itself is gone"
    );
}

/// Packing refs moves no value. git stamps those updates `REF_SKIP_CREATE_REFLOG`,
/// which `split_head_update()` checks before anything else, so `.git/logs/HEAD` must
/// not grow — and the run must not fail for want of a committer to stamp an entry with.
#[test]
fn packing_refs_adds_no_head_entry() {
    let f = Fixture::new("pack-refs");
    f.git(&["branch", "side"]);
    let head_before = f.log_lines("HEAD");

    f.git(&["pack-refs", "--all"]);

    assert_eq!(f.log_lines("HEAD"), head_before);
    assert_eq!(f.rev("refs/heads/main"), f.rev("HEAD"), "packing preserved the value");
}
