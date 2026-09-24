//! Which rows `fetch --dry-run` prints twice.
//!
//! Under `--dry-run` nothing is queued in the transaction (`s_update_ref()`
//! returns early, builtin/fetch.c:651-652), so `backfill_tags()`'s second
//! `find_non_local_tags()` call re-proposes every tag the first call proposed
//! and git prints those rows again. What it re-proposes is narrower than "every
//! tag row":
//!
//! * only tags this repository does not already have — `find_non_local_tags()`
//!   skips a name in `existing_refs` before either pass looks at it (:388-391);
//! * only as tag-following entries, `refs/tags/<t>` onto itself — a command-line
//!   refspec's row for the same tag under another name is not repeated, while one
//!   that maps the tag onto itself stands in for the entry `ref_remove_duplicates()`
//!   dropped, and is;
//! * and every repeated row is `not-for-merge`, so a repeated command-line row sorts
//!   with the others rather than ahead of them.
//!
//! Every expectation was measured against stock git 2.55.0 on this exact fixture.
//! No network: the remote is a directory next to the repository that fetches.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    /// Two commits; lightweight `v0.1.0` on the first, annotated `v0.2.0` on the second.
    up: PathBuf,
    /// A repository holding `up`'s objects but none of its tags, `origin` pointing at it.
    down: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-dryruntags-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (up, down) = (root.join("up"), root.join("down"));
        std::fs::create_dir_all(&up).unwrap();
        std::fs::create_dir_all(&down).unwrap();
        let f = Fixture { root, up, down };
        f.git(&f.up, &["init", "-q", "-b", "main"]);
        std::fs::write(f.up.join("a"), "a\n").unwrap();
        f.git(&f.up, &["add", "a"]);
        f.git(&f.up, &["commit", "-q", "-m", "a"]);
        f.git(&f.up, &["tag", "v0.1.0"]);
        std::fs::write(f.up.join("a"), "a\nb\n").unwrap();
        f.git(&f.up, &["commit", "-q", "-am", "b"]);
        f.git(&f.up, &["tag", "-a", "v0.2.0", "-m", "t"]);
        f.git(&f.down, &["init", "-q", "-b", "main"]);
        f.git(&f.down, &["remote", "add", "origin", f.up.to_str().unwrap()]);
        // Every object local, no tag: both tags are in reach of the first pass, so
        // the dry run's second pass has the whole set to repeat.
        f.git(&f.down, &["fetch", "-q", "--no-tags", "origin", "main:refs/remotes/origin/main"]);
        f
    }

    fn cmd(&self, dir: &Path, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C");
        c
    }

    fn git(&self, dir: &Path, args: &[&str]) {
        assert!(self.cmd(dir, args).status().unwrap().success(), "git {args:?} failed");
    }

    /// The summary rows of a `fetch --dry-run`, without the `From <url>` line.
    fn rows(&self, dir: &Path, args: &[&str]) -> Vec<String> {
        let mut argv = vec!["fetch", "--dry-run"];
        argv.extend_from_slice(args);
        let out = self.cmd(dir, &argv).output().unwrap();
        assert!(out.status.success(), "{argv:?}: {}", String::from_utf8_lossy(&out.stderr));
        let err = String::from_utf8(out.stderr).unwrap();
        err.lines().filter(|l| !l.starts_with("From ")).map(str::to_owned).collect()
    }
}

#[test]
fn tags_the_repository_already_has_are_not_repeated() {
    let f = Fixture::new("local");
    // `up` fetching from itself: both tags exist locally under their own names.
    let rows = f.rows(&f.up, &[".", "refs/tags/*:refs/tags/dryt/*"]);
    assert_eq!(
        rows,
        [
            " * [new tag]         v0.1.0     -> dryt/v0.1.0",
            " * [new tag]         v0.2.0     -> dryt/v0.2.0",
        ]
    );
}

#[test]
fn a_renamed_command_line_row_is_not_repeated() {
    let f = Fixture::new("renamed");
    let rows = f.rows(&f.down, &["origin", "refs/tags/v0.1.0:refs/tags/x"]);
    assert_eq!(
        rows,
        [
            " * [new tag]         v0.1.0     -> x",
            " * [new tag]         v0.1.0     -> v0.1.0",
            " * [new tag]         v0.2.0     -> v0.2.0",
            " * [new tag]         v0.1.0     -> v0.1.0",
            " * [new tag]         v0.2.0     -> v0.2.0",
        ]
    );
}

#[test]
fn a_self_mapped_command_line_row_is_repeated_as_not_for_merge() {
    let f = Fixture::new("self");
    let rows = f.rows(&f.down, &["origin", "tag", "v0.2.0"]);
    assert_eq!(
        rows,
        [
            " * [new tag]         v0.2.0     -> v0.2.0",
            " * [new tag]         v0.1.0     -> v0.1.0",
            " * [new tag]         v0.1.0     -> v0.1.0",
            " * [new tag]         v0.2.0     -> v0.2.0",
        ]
    );
}
