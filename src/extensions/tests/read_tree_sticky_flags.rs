//! Which index-entry flags survive `read-tree`.
//!
//! `oneway_merge()` (unpack-trees.c) answers `same(old, a)` with
//! `add_entry(o, old, update, CE_STAGEMASK)` — it re-adds the *existing* cache entry
//! rather than the one built from the tree, so every `ce_flags` bit but the stage
//! carries over. A plain `read-tree` runs no merge function at all and rebuilds the
//! index from the tree, which drops them.
//!
//! The bit that makes this visible is `CE_SKIP_WORKTREE`: losing it on a sparse
//! checkout makes `git status` report every excluded path as deleted.
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
    /// One commit with `keep.txt` and `hide.txt`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-rtflags-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        std::fs::write(f.work.join("keep.txt"), b"a\n").unwrap();
        std::fs::write(f.work.join("hide.txt"), b"b\n").unwrap();
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "one"]);
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

    fn stdout(&self, args: &[&str]) -> String {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// The `ls-files -t -v` tag of `path`: `H` plain, `S` skip-worktree,
    /// `h` assume-valid.
    fn tag(&self, path: &str) -> char {
        let listing = self.stdout(&["ls-files", "-t", "-v"]);
        listing
            .lines()
            .find_map(|l| l.strip_suffix(path).map(|t| t.trim_end().chars().next().unwrap()))
            .unwrap_or_else(|| panic!("no entry for {path} in:\n{listing}"))
    }
}

#[test]
fn merge_read_keeps_skip_worktree() {
    let f = Fixture::new("skip-m");
    f.git(&["update-index", "--skip-worktree", "hide.txt"]);
    assert_eq!(f.tag("hide.txt"), 'S');

    f.git(&["read-tree", "-m", "-u", "HEAD"]);

    assert_eq!(f.tag("hide.txt"), 'S', "`-m` re-adds the existing entry");
    assert_eq!(f.tag("keep.txt"), 'H');
    // The bit is what keeps a sparse-excluded path from being reported as deleted.
    assert_eq!(f.stdout(&["status", "--porcelain"]), "");
}

#[test]
fn reset_read_keeps_skip_worktree() {
    let f = Fixture::new("skip-reset");
    f.git(&["update-index", "--skip-worktree", "hide.txt"]);

    f.git(&["read-tree", "--reset", "-u", "HEAD"]);

    assert_eq!(f.tag("hide.txt"), 'S');
    assert_eq!(f.stdout(&["status", "--porcelain"]), "");
}

#[test]
fn merge_read_keeps_assume_valid() {
    let f = Fixture::new("assume");
    f.git(&["update-index", "--assume-unchanged", "hide.txt"]);
    assert_eq!(f.tag("hide.txt"), 'h');

    f.git(&["read-tree", "-m", "-u", "HEAD"]);

    assert_eq!(f.tag("hide.txt"), 'h');
}

/// The counterpart: a plain read installs the tree's own entries, so the flags go.
/// Keeping them here would be just as wrong as dropping them above.
#[test]
fn plain_read_drops_skip_worktree() {
    let f = Fixture::new("skip-plain");
    f.git(&["update-index", "--skip-worktree", "hide.txt"]);

    f.git(&["read-tree", "HEAD"]);

    assert_eq!(f.tag("hide.txt"), 'H', "no merge function ran, so nothing was carried");
}
