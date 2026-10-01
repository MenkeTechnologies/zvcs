//! A skip-worktree entry is out of a restore's reach only while the source leaves
//! it as it is.
//!
//! ```c
//! if (!opts->ignore_skipworktree && ce_skip_worktree(ce))
//!         return;
//! ```
//!
//! (`mark_ce_for_checkout_overlay()`/`_no_overlay()`, builtin/checkout.c:392, 426.)
//! The test runs *after* `read_tree_some()`, and `update_some()` keeps the old index
//! entry — flags and all — only when the tree names the same blob in the same mode
//! and the entry is not intent-to-add (builtin/checkout.c:214-229). Any other tree
//! entry is a fresh `create_ce_flags(0)` entry: no skip-worktree bit, so it is
//! matched, checked out and staged like any other path, and no assume-unchanged or
//! intent-to-add bit either.
//!
//! `restore` excluded every skip-worktree path up front for the pathspec check
//! (`--source=<changed> <sparse-path>` was "did not match") yet wrote them all from
//! a tree source (`--source=HEAD ..` materialized unchanged sparse files), and an
//! entry replaced by `--staged` kept its old flags.
//!
//! Every expectation below was measured against stock git 2.56.0.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    base: PathBuf,
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

impl Fixture {
    /// `in/a`, `out/b`, `top`, each changed by the second commit; a non-cone
    /// sparse checkout of `in` and `top` leaves `out/b` skip-worktree and absent.
    fn sparse(tag: &str) -> Self {
        let f = Self::empty(tag);
        f.write("in/a", "1\n");
        f.write("out/b", "2\n");
        f.write("top", "t\n");
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "base"]);
        f.write("in/a", "1b\n");
        f.write("out/b", "2b\n");
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "two"]);
        f.git(&["sparse-checkout", "set", "--no-cone", "in", "top"]);
        assert!(!f.exists("out/b"), "fixture: out/b must be outside the sparse checkout");
        assert_eq!(f.git(&["ls-files", "-t", "out/b"]).0, "S out/b\n");
        f
    }

    fn empty(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!("zvcs-rsps-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("repo");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(base.join("home")).unwrap();
        let f = Fixture { base, root };
        f.git(&["init", "-q", "-b", "main", "."]);
        f
    }

    fn write(&self, rel: &str, body: &str) {
        let p = self.root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    fn exists(&self, rel: &str) -> bool {
        self.root.join(rel).exists()
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.root.join(rel)).unwrap()
    }

    fn git(&self, args: &[&str]) -> (String, i32) {
        self.git_in(&self.root, args)
    }

    fn git_in(&self, dir: &Path, args: &[&str]) -> (String, i32) {
        let out = Command::new(BIN)
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("HOME", self.base.join("home"))
            .env("ZVCS_HOME", self.base.join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "2023-01-01 00:00:00 +0000")
            .env("GIT_COMMITTER_DATE", "2023-01-01 00:00:00 +0000")
            .output()
            .unwrap();
        let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
        s.push_str(&String::from_utf8_lossy(&out.stderr));
        (s, out.status.code().unwrap_or(-1))
    }

    /// Run from inside `in/`, as a user working in the sparse cone would.
    fn restore_from_in(&self, args: &[&str]) -> (String, i32) {
        let mut full = vec!["restore"];
        full.extend_from_slice(args);
        self.git_in(&self.root.join("in"), &full)
    }

    fn staged_blob(&self, path: &str) -> String {
        self.git(&["rev-parse", &format!(":{path}")]).0
    }

    fn blob(&self, rev_path: &str) -> String {
        self.git(&["rev-parse", rev_path]).0
    }
}

#[test]
fn a_sparse_path_the_source_changes_is_matched_and_checked_out() {
    let f = Fixture::sparse("changed-wt");
    let (out, rc) = f.restore_from_in(&["--source=HEAD~1", "../out/b"]);
    assert_eq!((out.as_str(), rc), ("", 0));
    assert_eq!(f.read("out/b"), "2\n");
    assert_eq!(f.staged_blob("out/b"), f.blob("HEAD:out/b"), "worktree-only: no index write");
}

#[test]
fn a_sparse_path_the_source_leaves_alone_stays_out_of_reach() {
    let f = Fixture::sparse("same-named");
    let (out, rc) = f.restore_from_in(&["--source=HEAD", "../out/b"]);
    assert_eq!(rc, 1, "got: {out}");
    assert_eq!(out, "error: pathspec '../out/b' did not match any file(s) known to git\n");
    assert!(!f.exists("out/b"));

    for args in [&["--source=HEAD", ".."][..], &["--source=HEAD", "--staged", "--worktree", ".."][..]] {
        let f = Fixture::sparse("same-all");
        let (out, rc) = f.restore_from_in(args);
        assert_eq!((out.as_str(), rc), ("", 0), "{args:?}");
        assert!(!f.exists("out/b"), "{args:?}: an unchanged sparse file must not be written");
        assert_eq!(f.git(&["ls-files", "-t", "out/b"]).0, "S out/b\n", "{args:?}");
    }
}

#[test]
fn staging_a_changed_sparse_path_drops_its_skip_worktree_bit() {
    for args in [
        &["--source=HEAD~1", "--staged", "../out"][..],
        &["--source=HEAD~1", "--staged", "--ignore-skip-worktree-bits", "../out/b"][..],
    ] {
        let f = Fixture::sparse("staged");
        let (out, rc) = f.restore_from_in(args);
        assert_eq!((out.as_str(), rc), ("", 0), "{args:?}");
        assert_eq!(f.staged_blob("out/b"), f.blob("HEAD~1:out/b"), "{args:?}");
        assert_eq!(f.git(&["ls-files", "-t", "out/b"]).0, "H out/b\n", "{args:?}");
        assert!(!f.exists("out/b"), "{args:?}: --staged alone writes no file");
    }
}

#[test]
fn a_replaced_entry_does_not_keep_assume_unchanged() {
    let f = Fixture::empty("assume");
    f.write("a", "a\n");
    f.git(&["add", "a"]);
    f.git(&["commit", "-qm", "base"]);
    f.git(&["checkout", "-qb", "side"]);
    f.write("a", "a2\n");
    f.git(&["commit", "-qam", "side"]);
    f.git(&["checkout", "-q", "main"]);
    f.git(&["update-index", "--assume-unchanged", "a"]);

    // Same blob: `update_some()` keeps the old entry, bit included.
    let (out, rc) = f.git(&["restore", "--source=HEAD", "--staged", "a"]);
    assert_eq!((out.as_str(), rc), ("", 0));
    assert_eq!(f.git(&["ls-files", "-v", "a"]).0, "h a\n");

    // Different blob: a fresh entry.
    let (out, rc) = f.git(&["restore", "--source=side", "--staged", "a"]);
    assert_eq!((out.as_str(), rc), ("", 0));
    assert_eq!(f.git(&["ls-files", "-v", "a"]).0, "H a\n");
}
