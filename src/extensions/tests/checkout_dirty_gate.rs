//! Which local changes stop a branch switch, and which are carried across.
//!
//! `unpack_trees()` with `twoway_merge` asks the question per path the two trees
//! disagree on — `verify_uptodate()` for a path being rewritten,
//! `verify_absent()` for one being added — and leaves every other path alone. A
//! blanket "is the worktree dirty" gate refuses switches git performs, which is
//! most of them: an edit to a file the target branch does not touch is normal.
//!
//! Submodules are never in the way: `verify_uptodate_1()` hands a gitlink to
//! `check_submodule_move_head()` and returns 0, so neither dirty content inside
//! the submodule nor a submodule `HEAD` that has moved stops the superproject
//! from switching — the gitlink in the index is updated and the submodule
//! worktree is left as it is.
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
    /// `main` and `other`, where `other` changes `b.txt` only.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-cogate-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        std::fs::write(f.work.join("a.txt"), "a\n").unwrap();
        std::fs::write(f.work.join("b.txt"), "b\n").unwrap();
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "base"]);
        f.git(&["checkout", "-q", "-b", "other"]);
        std::fs::write(f.work.join("b.txt"), "b-other\n").unwrap();
        f.git(&["commit", "-q", "-am", "bwork"]);
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

    /// `(exit code, stdout, stderr)`.
    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = self.cmd(args).output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn branch(&self) -> String {
        self.run(&["rev-parse", "--abbrev-ref", "HEAD"]).1.trim().to_string()
    }
}

/// An edit to a file the switch does not touch is carried across, not refused.
#[test]
fn an_unrelated_local_edit_does_not_stop_the_switch() {
    let f = Fixture::new("unrelated");
    std::fs::write(f.work.join("a.txt"), "a\nlocal\n").unwrap();

    let (code, out, err) = f.run(&["checkout", "other"]);
    assert_eq!(code, 0, "the switch should have happened: {out}{err}");
    assert_eq!(f.branch(), "other");
    assert_eq!(
        std::fs::read_to_string(f.work.join("a.txt")).unwrap(),
        "a\nlocal\n",
        "the local edit must survive the switch"
    );
    assert_eq!(std::fs::read_to_string(f.work.join("b.txt")).unwrap(), "b-other\n");
}

/// An edit to a file the switch *would* rewrite stops it, with git's wording —
/// note the advice says `switch branches`, not `checkout`.
#[test]
fn an_edit_in_the_way_refuses_with_gits_wording() {
    let f = Fixture::new("in-the-way");
    std::fs::write(f.work.join("b.txt"), "b\nlocal\n").unwrap();

    let (code, out, err) = f.run(&["checkout", "other"]);
    assert_eq!(code, 1, "the switch should have been refused: {out}{err}");
    assert_eq!(
        err,
        "error: Your local changes to the following files would be overwritten by checkout:\n\
         \tb.txt\n\
         Please commit your changes or stash them before you switch branches.\n\
         Aborting\n",
        "stderr: {err}"
    );
    assert_eq!(f.branch(), "main", "HEAD must not have moved");
    assert_eq!(std::fs::read_to_string(f.work.join("b.txt")).unwrap(), "b\nlocal\n");
}

/// An untracked file where the target branch has a tracked one is the other
/// half of the gate (`verify_absent()`).
#[test]
fn an_untracked_file_in_the_way_refuses() {
    let f = Fixture::new("untracked");
    f.git(&["checkout", "-q", "-b", "adds"]);
    std::fs::write(f.work.join("new.txt"), "tracked\n").unwrap();
    f.git(&["add", "new.txt"]);
    f.git(&["commit", "-q", "-m", "add new"]);
    f.git(&["checkout", "-q", "main"]);
    std::fs::write(f.work.join("new.txt"), "untracked\n").unwrap();

    let (code, out, err) = f.run(&["checkout", "adds"]);
    assert_eq!(code, 1, "the switch should have been refused: {out}{err}");
    assert_eq!(
        err,
        "error: The following untracked working tree files would be overwritten by checkout:\n\
         \tnew.txt\n\
         Please move or remove them before you switch branches.\n\
         Aborting\n",
        "stderr: {err}"
    );
    assert_eq!(f.branch(), "main");
}

/// A path both trees share keeps its index entry: `twoway_merge()` calls
/// `keep_entry()` for it, so a staged change to a file the target branch does
/// not touch survives the switch. Rebuilding the index from the target tree
/// would silently discard that work.
#[test]
fn a_staged_change_to_a_shared_path_survives_the_switch() {
    let f = Fixture::new("staged-kept");
    std::fs::write(f.work.join("a.txt"), "a\nstaged\n").unwrap();
    f.git(&["add", "a.txt"]);

    let (code, out, err) = f.run(&["checkout", "other"]);
    assert_eq!(code, 0, "the switch should have happened: {out}{err}");
    assert_eq!(f.branch(), "other");
    assert_eq!(
        std::fs::read_to_string(f.work.join("a.txt")).unwrap(),
        "a\nstaged\n",
        "the staged content must still be on disk"
    );
    let (_, staged, _) = f.run(&["show", ":a.txt"]);
    assert_eq!(staged, "a\nstaged\n", "the index entry must still hold it");
    // And `show_local_changes()` accounts for it, as git does.
    assert_eq!(out, "M\ta.txt\n", "stdout: {out}");
}
