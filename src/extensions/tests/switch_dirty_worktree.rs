//! What `git switch` does with a worktree that is not clean.
//!
//! It does exactly what `git checkout` does, because git implements both with
//! the same `merge_working_tree()`: a two-way `unpack_trees()` from the tree
//! `HEAD` holds onto the target's. A local modification to a path the two
//! branches agree on is carried across and listed on stdout; one to a path they
//! disagree on refuses the switch — in `checkout`'s wording, which `switch`
//! shares verbatim — and `--force`/`--discard-changes` resets instead.
//!
//! Refusing every dirty switch (the safe-but-narrow reading) makes `git switch`
//! unusable in the state people are in most of the time, and losing the local
//! change makes it dangerous. Neither is what git does.
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
    /// `main` and `other` differ in `b.txt` only; `a.txt` is identical on both.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-switchdirty-{tag}-{}", std::process::id()));
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
        f.git(&["add", "b.txt"]);
        f.git(&["commit", "-q", "-m", "other"]);
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

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.work.join(name)).unwrap()
    }
}

/// A modification to a path both branches share travels with the switch, and
/// `show_local_changes()` lists it on stdout.
#[test]
fn a_local_change_to_a_shared_path_travels_across() {
    let f = Fixture::new("carry");
    std::fs::write(f.work.join("a.txt"), "a\nlocal\n").unwrap();

    let (code, out, err) = f.run(&["switch", "other"]);
    assert_eq!(code, 0, "the switch should have happened: {out}{err}");
    assert_eq!(f.branch(), "other");
    assert_eq!(f.read("a.txt"), "a\nlocal\n", "the local change must survive");
    assert_eq!(f.read("b.txt"), "b-other\n", "the target's version must be checked out");
    assert_eq!(out, "M\ta.txt\n", "stdout: {out}");
    assert_eq!(err, "Switched to branch 'other'\n", "stderr: {err}");
}

/// A modification to a path the two branches disagree on stops the switch, with
/// `setup_unpack_trees_porcelain()`'s wording — which names `checkout` even when
/// the command was `switch`, since `switch` never sets its own.
#[test]
fn a_local_change_in_the_way_refuses_the_switch() {
    let f = Fixture::new("blocked");
    std::fs::write(f.work.join("b.txt"), "b-local\n").unwrap();

    let (code, out, err) = f.run(&["switch", "other"]);
    assert_eq!(code, 1, "the switch should have been refused: {out}{err}");
    assert_eq!(
        err,
        "error: Your local changes to the following files would be overwritten by checkout:\n\
         \tb.txt\n\
         Please commit your changes or stash them before you switch branches.\n\
         Aborting\n",
        "stderr: {err}"
    );
    assert_eq!(f.branch(), "main", "the branch must not have moved");
    assert_eq!(f.read("b.txt"), "b-local\n", "the local work must be untouched");
}

/// `--force` is `opts->discard_changes`: the blocked change is thrown away and
/// the switch happens, with no listing (there is nothing carried across).
#[test]
fn force_discards_the_blocking_change() {
    let f = Fixture::new("force");
    std::fs::write(f.work.join("b.txt"), "b-local\n").unwrap();

    let (code, out, err) = f.run(&["switch", "-f", "other"]);
    assert_eq!(code, 0, "the forced switch should have happened: {out}{err}");
    assert_eq!(f.branch(), "other");
    assert_eq!(f.read("b.txt"), "b-other\n");
    assert_eq!(out, "", "a discarding switch lists nothing: {out}");
}

/// `-c <new> <start>` and `--detach` go through the same gate, and a refusal
/// leaves no branch behind: `merge_working_tree()` runs before
/// `update_refs_for_switch()` creates anything.
#[test]
fn create_and_detach_share_the_gate() {
    let f = Fixture::new("create-detach");
    std::fs::write(f.work.join("b.txt"), "b-local\n").unwrap();

    let (code, out, err) = f.run(&["switch", "-c", "fresh", "other"]);
    assert_eq!(code, 1, "the create should have been refused: {out}{err}");
    assert!(err.contains("would be overwritten by checkout"), "stderr: {err}");
    assert_eq!(f.branch(), "main");
    assert_eq!(
        f.run(&["rev-parse", "--verify", "refs/heads/fresh"]).0,
        128,
        "no branch may be created by a refused switch"
    );

    let (code, out, err) = f.run(&["switch", "--detach", "other"]);
    assert_eq!(code, 1, "the detach should have been refused: {out}{err}");
    assert!(err.contains("would be overwritten by checkout"), "stderr: {err}");
    assert_eq!(f.branch(), "main", "HEAD must still be attached");
}

/// `-q` silences the transition message *and* the listing, as `opts->quiet`
/// does in git — the short spelling has to reach the option, not be swallowed.
#[test]
fn quiet_silences_the_listing_and_the_message() {
    let f = Fixture::new("quiet");
    std::fs::write(f.work.join("a.txt"), "a\nlocal\n").unwrap();

    let (code, out, err) = f.run(&["switch", "-q", "other"]);
    assert_eq!(code, 0, "the switch should have happened: {out}{err}");
    assert_eq!(f.branch(), "other");
    assert_eq!(out, "", "stdout: {out}");
    assert_eq!(err, "", "stderr: {err}");
}

/// Staged work on a shared path survives too — the two-way merge keeps the index
/// entry (`keep_entry()`), it does not rebuild the index from the target tree.
#[test]
fn staged_work_on_a_shared_path_survives() {
    let f = Fixture::new("staged");
    std::fs::write(f.work.join("a.txt"), "a\nstaged\n").unwrap();
    f.git(&["add", "a.txt"]);

    let (code, out, err) = f.run(&["switch", "other"]);
    assert_eq!(code, 0, "the switch should have happened: {out}{err}");
    assert_eq!(f.run(&["show", ":a.txt"]).1, "a\nstaged\n", "the index entry must survive");
    assert_eq!(f.read("a.txt"), "a\nstaged\n");
}
