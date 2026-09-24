//! `git merge --cleanup` / `commit.cleanup` are validated late.
//!
//! `OPT_CLEANUP` and the `commit.cleanup` config reader only store the string;
//! `get_cleanup_mode()` runs at builtin/merge.c:1498, after `--abort`, `--quit`
//! and `--continue` have been handled (:1417-1470) and after the
//! unfinished-merge refusals (:1475-1492). So `merge --abort` under
//! `commit.cleanup = bogus` aborts, and a stray argument next to `--abort` is
//! the usage error, not `Invalid cleanup mode`. zvcs refused the mode while
//! parsing.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.
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
    /// `main` and `theirs` both rewrite `file` from a common base.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-merge-cleanup-late-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file"), "base\n").unwrap();
        f.run(&["add", "file"]);
        f.run(&["commit", "-q", "-m", "base"]);
        f.run(&["checkout", "-q", "-b", "theirs"]);
        std::fs::write(f.work.join("file"), "theirs\n").unwrap();
        f.run(&["commit", "-q", "-am", "theirs"]);
        f.run(&["checkout", "-q", "main"]);
        std::fs::write(f.work.join("file"), "ours\n").unwrap();
        f.run(&["commit", "-q", "-am", "ours"]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("GIT_MERGE_AUTOEDIT", "no")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

/// A conflicted merge is aborted even though the configured mode is bogus.
#[test]
fn abort_ignores_an_invalid_commit_cleanup() {
    let f = Fixture::new("abort");
    let (_, _, code) = f.run(&["merge", "theirs"]);
    assert_eq!(code, 1);
    assert!(f.work.join(".git/MERGE_HEAD").exists());
    let (out, err, code) = f.run(&["-c", "commit.cleanup=bogus", "merge", "--abort"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "", 0));
    assert!(!f.work.join(".git/MERGE_HEAD").exists());
    assert_eq!(std::fs::read_to_string(f.work.join("file")).unwrap(), "ours\n");
}

/// `--abort` with operands is the usage error, whatever `--cleanup` says.
#[test]
fn abort_with_arguments_is_the_usage_error_before_the_mode() {
    let f = Fixture::new("args");
    let (out, err, code) = f.run(&["merge", "HEAD~1", "--cleanup=0x10", "--abort", "theirs"]);
    assert_eq!((out.as_str(), code), ("", 129));
    assert!(err.starts_with("fatal: --abort expects no arguments\n\nusage: git merge "), "{err:?}");
}

/// A real merge still dies on the mode, and `--no-cleanup` clears it first.
#[test]
fn a_merge_still_refuses_the_mode_unless_it_was_negated() {
    let f = Fixture::new("merge");
    let (out, err, code) = f.run(&["-c", "commit.cleanup=bogus", "merge", "theirs"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "fatal: Invalid cleanup mode bogus\n", 128));
    assert!(!f.work.join(".git/MERGE_HEAD").exists());

    let (_, _, code) = f.run(&["merge", "--cleanup=bogus", "--no-cleanup", "theirs"]);
    assert_eq!(code, 1);
    assert!(f.work.join(".git/MERGE_HEAD").exists());
}
