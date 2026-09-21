//! `git commit -m <msg> <path>` as the *initial* commit.
//!
//! A pathspec-limited commit builds a "false index" out of `HEAD`'s tree with
//! the matched paths replaced, but on an unborn branch there is no tree to build
//! on and git simply starts from nothing:
//!
//! ```c
//! static void create_base_index(const struct commit *current_head)
//! {
//!         if (!current_head) {
//!                 discard_index(the_repository->index);
//!                 return;
//!         }
//! ```
//! (builtin/commit.c:311-318, v2.55.0), with the matching
//! `list_paths(&partial, !current_head ? NULL : "HEAD", &pathspec)`
//! (builtin/commit.c:527) that skips the `overlay_tree_on_index()` so the
//! pathspecs are matched against the real index alone. The port refused the
//! whole form instead — `cannot do a pathspec-limited commit on an unborn
//! branch` — which is how git's own t1503 builds its repository.
//!
//! Every expectation was measured from stock git 2.55.0 in an identical
//! throwaway repository, stdout, stderr and exit status compared separately.
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
    /// An empty repository with `a` and `b` staged and no commit yet.
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("zvcs-initial-partial-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        std::fs::write(f.work.join("b"), "b\n").unwrap();
        f.git(&["add", "a", "b"]);
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

    fn stdout(&self, args: &[&str]) -> String {
        let (out, err, code) = self.run(args);
        assert_eq!((err.as_str(), code), ("", 0), "`git {args:?}`");
        out
    }
}

/// The commit is made, and it carries only the matched path: the other staged
/// path stays staged, exactly as a partial commit on a born branch leaves it.
#[test]
fn a_pathspec_limited_initial_commit_carries_only_the_matched_path() {
    let f = Fixture::new("one-path");
    let (_, _, code) = f.run(&["commit", "-q", "-m", "first", "a"]);
    assert_eq!(code, 0);

    assert_eq!(f.stdout(&["ls-tree", "--name-only", "HEAD"]), "a\n");
    assert_eq!(f.stdout(&["status", "--porcelain"]), "A  b\n");
    assert_eq!(f.stdout(&["rev-list", "--count", "HEAD"]), "1\n");
}

/// Several pathspecs, and a directory pathspec, go the same way: the false index
/// starts empty and `add_remove_files()` puts back exactly what matched.
#[test]
fn a_pathspec_limited_initial_commit_takes_every_matched_path() {
    let f = Fixture::new("dir");
    std::fs::create_dir(f.work.join("d")).unwrap();
    std::fs::write(f.work.join("d/c"), "c\n").unwrap();
    f.git(&["add", "d/c"]);

    let (_, _, code) = f.run(&["commit", "-q", "-m", "first", "a", "d"]);
    assert_eq!(code, 0);

    assert_eq!(f.stdout(&["ls-tree", "-r", "--name-only", "HEAD"]), "a\nd/c\n");
    assert_eq!(f.stdout(&["status", "--porcelain"]), "A  b\n");
}

/// A pathspec that matches nothing in the index is still the refusal git gives
/// — the unborn shortcut must not turn an empty match into an empty commit.
#[test]
fn a_pathspec_matching_nothing_is_still_refused_on_an_unborn_branch() {
    let f = Fixture::new("nomatch");
    let (_, err, code) = f.run(&["commit", "-q", "-m", "first", "nope"]);
    assert_ne!(code, 0, "{err:?}");
    let (_, _, code) = f.run(&["rev-parse", "--verify", "HEAD"]);
    assert_ne!(code, 0, "no commit was made");
}
