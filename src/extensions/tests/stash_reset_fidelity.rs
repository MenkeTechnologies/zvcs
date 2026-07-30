//! What `git stash push` leaves on disk after it has stored the stash.
//!
//! Every expectation here was taken from stock git 2.55.0 against the same
//! fixture, and each one covers a way the reset can silently lose or keep too
//! much:
//!
//! * `-S` subtracts the *staged* diff from the worktree (`git apply -R`), so an
//!   unstaged edit to the same file survives — and when the two overlap, git's
//!   `apply` is all-or-nothing: nothing is written and the command fails with
//!   the stash entry still in place.
//! * A mode change is part of the reset: `reset --hard` puts the tree's
//!   executable bit back, so a `chmod +x` does not survive a push that reported
//!   the tree saved.
//! * `-u` removes the captured untracked files with `clean --force -d`, whose
//!   `-d` also takes the directories they leave empty.
//!
//! Unix-only: the mode assertions are about the executable bit.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-stashreset-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.write("a.txt", "l1\nl2\nl3\n");
        f.write("b.txt", "b\n");
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "base"]);
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

    fn write(&self, path: &str, body: &str) {
        let full = self.work.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(full, body).unwrap();
    }

    fn read(&self, path: &str) -> String {
        std::fs::read_to_string(self.work.join(path)).unwrap()
    }

    fn status(&self) -> Vec<(String, String)> {
        let out = self.cmd(&["status", "--porcelain"]).output().unwrap();
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.contains(".zvcs/"))
            .map(|l| (l[..2].to_string(), l[3..].to_string()))
            .collect()
    }

    fn is_executable(&self, path: &str) -> bool {
        let meta = std::fs::metadata(self.work.join(path)).unwrap();
        meta.permissions().mode() & 0o111 != 0
    }
}

/// `-S` takes the staged diff and *only* the staged diff: an unstaged edit to a
/// different line of the same file is neither stashed nor deleted, it stays on
/// disk. Reverting the whole path to `HEAD` here would destroy work that no
/// stash entry holds.
#[test]
fn staged_only_push_keeps_the_unstaged_edit_on_disk() {
    let f = Fixture::new("staged-keeps");
    f.write("a.txt", "STAGED\nl2\nl3\n");
    f.git(&["add", "a.txt"]);
    f.write("a.txt", "STAGED\nl2\nWORKTREE\n");

    let (ok, out, err) = f.run(&["stash", "push", "-S", "-m", "s"]);
    assert!(ok, "stash -S failed: {out}{err}");

    // The staged hunk is gone from the file, the unstaged one is still there.
    assert_eq!(f.read("a.txt"), "l1\nl2\nWORKTREE\n");
    assert_eq!(f.status(), [(" M".to_string(), "a.txt".to_string())]);
    // And the stash holds the staged state, so nothing was lost either way.
    let (_, stashed, _) = f.run(&["show", "stash@{0}:a.txt"]);
    assert_eq!(stashed, "STAGED\nl2\nl3\n");
}

/// When the unstaged edit overlaps the staged one, git's `apply -R` cannot place
/// the reverse patch. It is all-or-nothing, so the worktree and index are left
/// exactly as they were — and the stash entry, already written, stays.
#[test]
fn staged_only_push_refuses_when_the_reverse_patch_does_not_apply() {
    let f = Fixture::new("staged-conflict");
    f.write("a.txt", "STAGED\nl2\nl3\n");
    f.git(&["add", "a.txt"]);
    f.write("a.txt", "STAGED-THEN-EDITED\nl2\nl3\n");

    let (ok, out, err) = f.run(&["stash", "push", "-S", "-m", "s"]);
    assert!(!ok, "overlapping edit must fail: {out}{err}");
    assert!(
        err.contains("Cannot remove worktree changes"),
        "git's refusal is not reported: {err}"
    );
    // The stash was stored before the reset was attempted, so it is announced.
    assert!(out.contains("Saved working directory and index state"), "stdout: {out}");

    assert_eq!(f.read("a.txt"), "STAGED-THEN-EDITED\nl2\nl3\n");
    assert_eq!(f.status(), [("MM".to_string(), "a.txt".to_string())]);
    let (_, list, _) = f.run(&["stash", "list"]);
    assert!(list.contains("On main: s"), "the entry must be kept: {list}");
}

/// A mode change is a change: after the push the tree is clean, which means the
/// executable bit came back off. Restoring only the *content* would leave the
/// repository dirty right after a push reported it saved.
#[test]
fn push_restores_the_file_mode() {
    let f = Fixture::new("mode");
    let mut perms = std::fs::metadata(f.work.join("a.txt")).unwrap().permissions();
    perms.set_mode(perms.mode() | 0o111);
    std::fs::set_permissions(f.work.join("a.txt"), perms).unwrap();
    assert!(f.is_executable("a.txt"), "fixture failed to set the bit");

    let (ok, out, err) = f.run(&["stash", "push", "-m", "m"]);
    assert!(ok, "stash failed: {out}{err}");

    assert!(!f.is_executable("a.txt"), "the executable bit survived the reset");
    assert_eq!(f.status(), [] as [(String, String); 0]);
}

/// `-u` deletes the untracked files it captured with `clean --force -d`, and
/// `-d` takes the emptied directories with them — while a directory that still
/// holds something stays.
#[test]
fn include_untracked_removes_the_directories_it_empties() {
    let f = Fixture::new("untracked-dirs");
    f.write("keep/tracked.txt", "k\n");
    f.git(&["add", "keep/tracked.txt"]);
    f.git(&["commit", "-q", "-m", "keep"]);
    f.write("nested/deep/new.txt", "u\n");
    f.write("keep/also-new.txt", "u\n");
    f.write("a.txt", "changed\n");

    let (ok, out, err) = f.run(&["stash", "push", "-u", "-m", "u"]);
    assert!(ok, "stash -u failed: {out}{err}");

    assert!(!f.work.join("nested").exists(), "the emptied directory tree is still there");
    assert!(f.work.join("keep").is_dir(), "a directory with tracked content must stay");
    assert!(!f.work.join("keep/also-new.txt").exists(), "the untracked file was not removed");

    // And the pop puts every one of them back.
    let (ok, out, err) = f.run(&["stash", "pop"]);
    assert!(ok, "stash pop failed: {out}{err}");
    assert_eq!(f.read("nested/deep/new.txt"), "u\n");
    assert_eq!(f.read("keep/also-new.txt"), "u\n");
}
