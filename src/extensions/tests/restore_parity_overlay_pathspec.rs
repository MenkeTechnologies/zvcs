//! In overlay mode with a source tree, only entries the tree supplied can satisfy a
//! pathspec.
//!
//! ```c
//! if (opts->source_tree && !(ce->ce_flags & CE_UPDATE))
//!         return;
//! ...
//! if (ce_path_match(the_repository->index, ce, &opts->pathspec, ps_matched))
//!         ce->ce_flags |= CE_MATCHED;
//! ```
//!
//! (`mark_ce_for_checkout_overlay()`, builtin/checkout.c:394-418.) `CE_UPDATE` is
//! stamped by `update_some()` (builtin/checkout.c:193) on each entry `read_tree_some()`
//! takes from the source, so a path that exists only in the index never reaches
//! `ce_path_match()` and `report_path_error()` names it. `restore` checked every spec
//! against the union of source and index paths regardless of `--overlay`, so
//! `git restore --overlay --staged <newly-added>` exited 0 and, alongside another
//! spec, went on to restore the paths that did match.
//!
//! Every expectation below was measured against stock git 2.56.0.
#![cfg(unix)]

use std::path::PathBuf;
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
    /// `t.txt` committed as `one`, then changed to `two` and staged; `added.txt`
    /// staged but absent from HEAD.
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!("zvcs-rovl-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("repo");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(base.join("home")).unwrap();
        let f = Fixture { base, root };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.write("t.txt", "one\n");
        f.git(&["add", "t.txt"]);
        f.git(&["commit", "-qm", "one"]);
        f.write("t.txt", "two\n");
        f.write("added.txt", "new\n");
        f.git(&["add", "t.txt", "added.txt"]);
        f
    }

    fn write(&self, rel: &str, body: &str) {
        std::fs::write(self.root.join(rel), body).unwrap();
    }

    fn status(&self) -> String {
        self.git(&["status", "--porcelain=v1", "-uall"]).0
    }

    fn git(&self, args: &[&str]) -> (String, i32) {
        let out = Command::new(BIN)
            .arg("-C")
            .arg(&self.root)
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
}

const UNTOUCHED: &str = "A  added.txt\nM  t.txt\n";

#[test]
fn an_index_only_path_does_not_match_with_a_source_tree() {
    for args in [
        &["restore", "--overlay", "--staged", "added.txt"][..],
        &["restore", "--overlay", "--source=HEAD", "added.txt"][..],
        &["restore", "--overlay", "--source=HEAD", "--staged", "--worktree", "added.txt"][..],
    ] {
        let f = Fixture::new("plain");
        let (out, rc) = f.git(args);
        assert_eq!(rc, 1, "{args:?}: report_path_error() exits 1, got: {out}");
        assert_eq!(
            out, "error: pathspec 'added.txt' did not match any file(s) known to git\n",
            "{args:?}"
        );
        assert_eq!(f.status(), UNTOUCHED, "{args:?}: nothing may change");
    }
}

#[test]
fn the_unmatched_spec_stops_the_whole_restore() {
    let f = Fixture::new("mixed");
    let (out, rc) = f.git(&["restore", "--overlay", "--staged", "added.txt", "t.txt"]);
    assert_eq!(rc, 1, "got: {out}");
    assert_eq!(out, "error: pathspec 'added.txt' did not match any file(s) known to git\n");
    assert_eq!(f.status(), UNTOUCHED, "t.txt must stay staged: the check precedes any write");
}

#[test]
fn a_glob_matching_only_index_paths_does_not_match() {
    let f = Fixture::new("glob");
    let (out, rc) = f.git(&["restore", "--overlay", "--staged", "add*"]);
    assert_eq!(rc, 1, "got: {out}");
    assert_eq!(out, "error: pathspec 'add*' did not match any file(s) known to git\n");
    assert_eq!(f.status(), UNTOUCHED);
}

#[test]
fn without_a_source_tree_or_overlay_the_index_path_still_matches() {
    // No source tree: the index is the source, every entry is eligible.
    let f = Fixture::new("noscr");
    let (out, rc) = f.git(&["restore", "--overlay", "added.txt"]);
    assert_eq!((out.as_str(), rc), ("", 0));
    assert_eq!(f.status(), UNTOUCHED);

    // No-overlay: `mark_ce_for_checkout_no_overlay()` matches first, then removes.
    let f = Fixture::new("noovl");
    let (out, rc) = f.git(&["restore", "--staged", "added.txt"]);
    assert_eq!((out.as_str(), rc), ("", 0));
    assert_eq!(f.status(), "M  t.txt\n?? added.txt\n");

    // A spec the source does satisfy keeps the index-only path, as overlay should.
    let f = Fixture::new("dot");
    let (out, rc) = f.git(&["restore", "--overlay", "--staged", "."]);
    assert_eq!((out.as_str(), rc), ("", 0));
    assert_eq!(f.status(), "A  added.txt\n M t.txt\n");
}
