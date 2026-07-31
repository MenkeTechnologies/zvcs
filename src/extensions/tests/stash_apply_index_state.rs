//! The index a plain `git stash apply` leaves behind.
//!
//! `do_apply_stash()` finishes with `unstage_changes_unless_new()`, whose name
//! is the whole rule: the merge stages everything it resolved, and git then
//! restores each path's entry from the pre-apply index tree — but only for paths
//! that *existed* there (`if (p->one->oid_valid)`). A file the stash added is
//! not one of them, so its merged entry survives and the file comes back
//! **staged**.
//!
//! Getting this wrong is not cosmetic: a new file that comes back untracked is
//! skipped by `git commit -a`, so the change silently misses the commit.
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
    /// A stash holding one modified tracked file and one newly added file.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-stashidx-{tag}-{}", std::process::id()));
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

        std::fs::write(f.work.join("a.txt"), "modified\n").unwrap();
        std::fs::write(f.work.join("new.txt"), "new\n").unwrap();
        f.git(&["add", "new.txt"]);
        f.git(&["stash", "push", "-q", "-m", "s"]);
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

    fn status(&self) -> Vec<(String, String)> {
        let out = self.cmd(&["status", "--porcelain"]).output().unwrap();
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.contains(".zvcs/"))
            .map(|l| (l[..2].to_string(), l[3..].to_string()))
            .collect()
    }
}

/// Without `--index`: the tracked file comes back unstaged, the added file comes
/// back staged.
#[test]
fn apply_unstages_tracked_changes_but_keeps_new_files_staged() {
    let f = Fixture::new("apply");
    let (ok, out, err) = f.run(&["stash", "apply", "-q"]);
    assert!(ok, "apply failed: {out}{err}");

    let status = f.status();
    assert!(
        status.contains(&(" M".to_string(), "a.txt".to_string())),
        "a tracked change must come back unstaged: {status:?}"
    );
    assert!(
        status.contains(&("A ".to_string(), "new.txt".to_string())),
        "a path the stash added has nothing to unstage to, so it stays staged: {status:?}"
    );
}

/// `pop` restores the same state, then drops the entry.
#[test]
fn pop_leaves_the_same_index_state() {
    let f = Fixture::new("pop");
    let (ok, out, err) = f.run(&["stash", "pop", "-q"]);
    assert!(ok, "pop failed: {out}{err}");

    let status = f.status();
    assert!(status.contains(&("A ".to_string(), "new.txt".to_string())), "status: {status:?}");
    let (_, list, _) = f.run(&["stash", "list"]);
    assert!(list.is_empty(), "the entry should be gone: {list}");
}

/// `--index` is the other branch: the stash's own staged state is restored, so
/// the tracked change is staged again too.
#[test]
fn apply_with_index_restores_the_staged_state() {
    let f = Fixture::new("apply-index");
    let (ok, out, err) = f.run(&["stash", "apply", "--index", "-q"]);
    assert!(ok, "apply --index failed: {out}{err}");

    let status = f.status();
    assert!(status.contains(&("A ".to_string(), "new.txt".to_string())), "status: {status:?}");
    assert!(
        status.contains(&(" M".to_string(), "a.txt".to_string())),
        "a.txt was never staged in the stash, so it stays unstaged: {status:?}"
    );
}

/// `--index` replays the stash's staged state onto the current index first, and
/// when that cannot be done git stops there: the error, the kept entry, and a
/// worktree that has not been touched.
#[test]
fn apply_with_index_refuses_before_touching_the_worktree() {
    let f = Fixture::new("index-conflict");
    // Stage a different content for the same path the stash staged.
    std::fs::write(f.work.join("new.txt"), "local\n").unwrap();
    f.git(&["add", "new.txt"]);
    let before = f.status();

    let (ok, out, err) = f.run(&["stash", "pop", "--index"]);
    assert!(!ok, "the index apply should fail: {out}{err}");
    assert!(
        err.contains("error: conflicts in index. Try without --index."),
        "stderr: {err}"
    );
    assert!(
        out.contains("The stash entry is kept in case you need it again."),
        "stdout: {out}"
    );
    assert_eq!(f.status(), before, "nothing may move when the index apply is refused");
    assert_eq!(std::fs::read_to_string(f.work.join("new.txt")).unwrap(), "local\n");
    let (_, list, _) = f.run(&["stash", "list"]);
    assert_eq!(list.lines().count(), 1, "the entry must survive: {list}");
}

/// A stash that cannot be applied because the worktree holds work on one of its
/// paths: `merge_switch_to_result()`'s `checkout()` fails, so it returns before
/// `merge_display_update_messages()` — nothing about the merge is printed, only
/// the refusal. Announcing `Auto-merging a.txt` before `Aborting` would claim a
/// merge that never reached the disk.
#[test]
fn a_refused_apply_prints_the_refusal_and_nothing_about_the_merge() {
    let f = Fixture::new("refused-quiet");
    std::fs::write(f.work.join("a.txt"), "local work\n").unwrap();

    let (ok, out, err) = f.run(&["stash", "apply", "-q"]);
    assert!(!ok, "the apply should have been refused: {out}{err}");
    assert_eq!(out, "", "nothing may be reported about the merge: {out}");
    assert_eq!(
        err,
        "error: Your local changes to the following files would be overwritten by merge:\n\
         \ta.txt\n\
         Please commit your changes or stash them before you merge.\n\
         Aborting\n",
        "stderr: {err}"
    );
    assert_eq!(
        std::fs::read_to_string(f.work.join("a.txt")).unwrap(),
        "local work\n",
        "the local work must be untouched"
    );
}

/// `--index` asked for the stash's staged state; a merge that fails never gets
/// there, and git says so on stderr before moving on to the untracked files.
#[test]
fn a_failed_apply_with_index_says_the_index_was_not_unstashed() {
    let f = Fixture::new("index-not-unstashed");
    std::fs::write(f.work.join("a.txt"), "local work\n").unwrap();

    let (ok, out, err) = f.run(&["stash", "apply", "--index", "-q"]);
    assert!(!ok, "the apply should have been refused: {out}{err}");
    assert!(
        err.ends_with("Aborting\nIndex was not unstashed.\n"),
        "stderr: {err}"
    );
}

/// A conflicted apply still hands the untracked files back: `do_apply_stash()`
/// jumps to `restore_untracked`, it does not return. Leaving them in the stash
/// would hide files the user has no reason to suspect are still there.
#[test]
fn a_conflicted_apply_still_restores_untracked_files() {
    let f = Fixture::new("conflict-untracked");
    // A stash carrying both a tracked edit and an untracked file...
    f.git(&["stash", "drop", "-q"]);
    std::fs::write(f.work.join("a.txt"), "stashed\n").unwrap();
    std::fs::write(f.work.join("u.txt"), "untracked\n").unwrap();
    f.git(&["stash", "push", "-q", "-u", "-m", "u"]);
    // ...and a HEAD that has moved onto the same line since.
    std::fs::write(f.work.join("a.txt"), "upstream\n").unwrap();
    f.git(&["add", "a.txt"]);
    f.git(&["commit", "-q", "-m", "moved"]);

    let (ok, out, err) = f.run(&["stash", "apply", "-q"]);
    assert!(!ok, "the apply should have conflicted: {out}{err}");
    assert_eq!(
        std::fs::read_to_string(f.work.join("u.txt")).unwrap(),
        "untracked\n",
        "the untracked file must come back even though the merge conflicted"
    );
    assert!(
        std::fs::read_to_string(f.work.join("a.txt")).unwrap().contains("<<<<<<<"),
        "the conflict must be left in the worktree"
    );
}
