//! `stash drop`/`clear` go through the ref store, not the filesystem.
//!
//! `refs/stash` is an ordinary reference and `git pack-refs --all` moves it into
//! `packed-refs`, where unlinking `.git/refs/stash` does nothing: the packed
//! entry keeps resolving, so the "dropped" stash commit stays reachable, `gc`
//! never prunes it, and `git rev-parse refs/stash` still answers. git's
//! `do_clear_stash()` is `delete_ref()`, which removes it wherever it lives.
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
    /// A repository with two packed stash entries.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-stashref-{tag}-{}", std::process::id()));
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
        std::fs::write(f.work.join("a.txt"), "one\n").unwrap();
        f.git(&["stash", "push", "-q", "-m", "one"]);
        std::fs::write(f.work.join("a.txt"), "two\n").unwrap();
        f.git(&["stash", "push", "-q", "-m", "two"]);
        f.git(&["pack-refs", "--all"]);
        assert!(!f.work.join(".git/refs/stash").exists(), "fixture did not pack refs/stash");
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

    /// Whether `refs/stash` still resolves, by any storage backend.
    fn stash_ref_resolves(&self) -> bool {
        self.cmd(&["rev-parse", "--verify", "refs/stash"])
            .output()
            .unwrap()
            .status
            .success()
    }

    fn list(&self) -> Vec<String> {
        let out = self.cmd(&["stash", "list"]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).lines().map(str::to_owned).collect()
    }
}

/// Dropping the last entry deletes the ref itself — including a packed one.
#[test]
fn dropping_the_last_entry_deletes_a_packed_ref() {
    let f = Fixture::new("drop");
    let (ok, out, err) = f.run(&["stash", "drop"]);
    assert!(ok, "drop failed: {out}{err}");
    // One entry left, and the ref now points at it.
    assert_eq!(f.list().len(), 1, "one entry should remain: {:?}", f.list());
    assert!(f.stash_ref_resolves(), "refs/stash must still resolve while entries remain");

    let (ok, out, err) = f.run(&["stash", "drop"]);
    assert!(ok, "second drop failed: {out}{err}");
    assert!(f.list().is_empty(), "the list should be empty: {:?}", f.list());
    assert!(
        !f.stash_ref_resolves(),
        "the packed refs/stash outlived the drop, keeping the stash commit reachable"
    );
}

/// `clear` deletes the ref wherever it is stored, so nothing keeps the cleared
/// commits alive.
#[test]
fn clear_deletes_a_packed_ref() {
    let f = Fixture::new("clear");
    let (ok, out, err) = f.run(&["stash", "clear"]);
    assert!(ok, "clear failed: {out}{err}");
    assert!(f.list().is_empty(), "the list should be empty: {:?}", f.list());
    assert!(!f.stash_ref_resolves(), "the packed refs/stash outlived the clear");
}
