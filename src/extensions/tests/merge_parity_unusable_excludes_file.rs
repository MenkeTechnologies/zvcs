//! `git merge` under a `core.excludesFile` that names a directory.
//!
//! Every `unpack_trees()` caller `cmd_merge` reaches leaves `preserve_ignored`
//! at 0, so `setup_standard_excludes()` runs and `add_patterns_from_file_1()`
//! dies with `cannot use <path> as an exclude file` (unpack-trees.c:1922-1926,
//! dir.c:1323-1324) before a tree is read:
//!
//! * `--abort` — the `reset --merge` it runs (builtin/merge.c:1436,
//!   builtin/reset.c:83) dies with the merge state intact;
//! * a fast-forward — `checkout_fast_forward()` dies after `Updating a..b`;
//! * a real merge — `save_state()`'s `stash create` child dies in
//!   `check_changes_tracked_files()`, and the parent adds `stash failed`
//!   (builtin/merge.c:365-366);
//! * a clean `--autostash` — `create_autostash_ref()`'s
//!   `has_uncommitted_changes()` dies first, with no `stash failed`.
//!
//! Every expectation was measured from stock git 2.56.0 in an identical
//! throwaway repository under the same pinned environment.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");
const FATAL: &str = "fatal: cannot use src as an exclude file\n";

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
    /// `main` (`i` then `m`), `ff` one commit ahead of it, and `side` forked
    /// from `i` with a conflicting edit to `f`. `src/` is a tracked directory,
    /// which is what `core.excludesFile=src` will name.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-merge-excl-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(work.join("src")).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("src/x"), "a\n").unwrap();
        std::fs::write(f.work.join("f"), "b\n").unwrap();
        f.git(&["add", "."]);
        f.git(&["commit", "-q", "-m", "i"]);
        f.git(&["branch", "side"]);
        std::fs::write(f.work.join("f"), "d\n").unwrap();
        f.git(&["commit", "-q", "-a", "-m", "m"]);
        f.git(&["checkout", "-q", "-b", "ff"]);
        std::fs::write(f.work.join("g"), "z\n").unwrap();
        f.git(&["add", "g"]);
        f.git(&["commit", "-q", "-m", "ff"]);
        f.git(&["checkout", "-q", "side"]);
        std::fs::write(f.work.join("f"), "c\n").unwrap();
        f.git(&["commit", "-q", "-a", "-m", "s"]);
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
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = self.cmd(args).output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn oid(&self, spec: &str) -> String {
        let (out, _, _) = self.run(&["rev-parse", "--short", spec]);
        out.trim().to_string()
    }

    fn break_excludes(&self) {
        self.git(&["config", "core.excludesFile", "src"]);
    }
}

/// `--abort` dies in `reset --merge` and leaves the conflicted merge as it
/// was: `MERGE_HEAD` stays, the index keeps its three stages.
#[test]
fn abort_dies_and_keeps_the_conflicted_merge() {
    let f = Fixture::new("abort");
    let (_, _, code) = f.run(&["merge", "side"]);
    assert_eq!(code, 1);
    f.break_excludes();

    assert_eq!(f.run(&["merge", "--abort"]), (String::new(), FATAL.to_string(), 128));
    assert!(f.work.join(".git/MERGE_HEAD").exists());
    let (unmerged, _, _) = f.run(&["ls-files", "-u"]);
    assert_eq!(unmerged.lines().count(), 3, "{unmerged}");
}

/// A fast-forward prints `Updating a..b` and then dies in the checkout; the
/// branch does not move. `-q` drops the `Updating` line only.
#[test]
fn fast_forward_dies_after_the_updating_line() {
    let f = Fixture::new("ff");
    let head = f.oid("HEAD");
    let ff = f.oid("ff");
    f.break_excludes();

    let updating = format!("Updating {head}..{ff}\n");
    assert_eq!(f.run(&["merge", "ff"]), (updating.clone(), FATAL.to_string(), 128));
    assert_eq!(f.run(&["merge", "--squash", "ff"]), (updating.clone(), FATAL.to_string(), 128));
    assert_eq!(f.run(&["merge", "--autostash", "ff"]), (updating, FATAL.to_string(), 128));
    assert_eq!(f.run(&["merge", "-q", "ff"]), (String::new(), FATAL.to_string(), 128));
    assert_eq!(f.oid("HEAD"), head);
}

/// A real merge dies in `save_state()`: the `stash create` child's fatal, then
/// the parent's `stash failed`. A clean `--autostash` dies one step earlier,
/// with the child's line alone. The trivial in-index pre-pass of `-s resolve`
/// dies in `repo_index_has_changes()` before it prints anything.
#[test]
fn real_merge_dies_in_save_state() {
    let f = Fixture::new("strategy");
    let head = f.oid("HEAD");
    f.break_excludes();

    let stash_failed = format!("{FATAL}fatal: stash failed\n");
    assert_eq!(f.run(&["merge", "-m", "x", "side"]), (String::new(), stash_failed.clone(), 128));
    assert_eq!(f.run(&["merge", "--no-ff", "-m", "x", "ff"]), (String::new(), stash_failed, 128));
    assert_eq!(f.run(&["merge", "--autostash", "-m", "x", "side"]), (String::new(), FATAL.to_string(), 128));
    assert_eq!(f.run(&["merge", "-s", "resolve", "-m", "x", "side"]), (String::new(), FATAL.to_string(), 128));
    assert_eq!(f.oid("HEAD"), head);
    assert!(!f.work.join(".git/MERGE_HEAD").exists());
}
