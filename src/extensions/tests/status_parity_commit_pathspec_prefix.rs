//! `git commit <pathspec>` run below the top of the work tree.
//!
//! `prepare_index()` parses the operands with `parse_pathspec(&pathspec, 0,
//! PATHSPEC_PREFER_FULL, prefix, argv)` (builtin/commit.c:365-367), so every
//! spec is spelled from the top of the work tree before `list_paths()` compares
//! it with the index: `prefix_path()` joins the current directory's prefix onto
//! it and resolves `.` and `..` away. `report_path_error()` then names the spec
//! *as typed* for whatever still matched nothing.
//!
//! `list_paths()` matches against the index with HEAD overlaid
//! (`overlay_tree_on_index()`, :266-270), so a path that only HEAD still has —
//! one `git rm` took out of the index — is a path git knows.
//!
//! Every expectation was measured from stock git 2.55.0 in an identical
//! throwaway repository under the same pinned environment.
#![cfg(unix)]

use std::path::{Path, PathBuf};
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
    /// `top.txt` at the root and `sub/inner.txt` below it, both committed.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-st-prefix-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(work.join("sub")).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("top.txt"), "one\n").unwrap();
        std::fs::write(f.work.join("sub/inner.txt"), "one\n").unwrap();
        f.git(&["add", "."]);
        f.git(&["commit", "-q", "-m", "first"]);
        f
    }

    fn cmd_in(&self, dir: &Path, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
            .env("ZVCS_HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("GIT_EDITOR", ":")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd_in(&self.work, args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn run_in(&self, dir: &Path, args: &[&str]) -> (String, String, i32) {
        let out = self.cmd_in(dir, args).output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    /// The paths the tip commit changed.
    fn changed(&self) -> String {
        let out = self
            .cmd_in(&self.work, &["show", "--name-only", "--format=", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn sub(&self) -> PathBuf {
        self.work.join("sub")
    }
}

/// A spec naming a file in the current directory is resolved against the prefix,
/// not read as a top-level path.
#[test]
fn a_spec_from_a_subdirectory_is_joined_to_the_prefix() {
    let f = Fixture::new("plain");
    std::fs::write(f.work.join("sub/inner.txt"), "two\n").unwrap();
    std::fs::write(f.work.join("top.txt"), "two\n").unwrap();
    let (_, err, code) = f.run_in(&f.sub(), &["commit", "-m", "second", "inner.txt"]);
    assert_eq!((code, err.as_str()), (0, ""), "{err}");
    assert_eq!(f.changed(), "sub/inner.txt\n", "the wrong path was committed");
}

/// `..` climbs out of the prefix.
#[test]
fn a_spec_may_climb_out_of_the_prefix() {
    let f = Fixture::new("dotdot");
    std::fs::write(f.work.join("top.txt"), "two\n").unwrap();
    let (_, err, code) = f.run_in(&f.sub(), &["commit", "-m", "second", "../top.txt"]);
    assert_eq!((code, err.as_str()), (0, ""), "{err}");
    assert_eq!(f.changed(), "top.txt\n");
}

/// A path `git rm` took out of the index is still a path git knows, because
/// `list_paths()` overlays HEAD on the index before matching.
#[test]
fn a_removed_path_still_matches_from_a_subdirectory() {
    let f = Fixture::new("removed");
    f.git(&["rm", "-q", "top.txt"]);
    let (_, err, code) = f.run_in(&f.sub(), &["commit", "-m", "second", "../top.txt"]);
    assert_eq!((code, err.as_str()), (0, ""), "{err}");
    assert_eq!(f.changed(), "top.txt\n");
}

/// A spec that matches nothing is still reported as typed, and the commit is
/// refused with status 1.
#[test]
fn an_unmatched_spec_is_reported_as_typed() {
    let f = Fixture::new("unmatched");
    let (_, err, code) = f.run_in(&f.sub(), &["commit", "-m", "second", "nope.txt"]);
    assert_eq!(code, 1, "{err:?}");
    assert!(
        err.contains("error: pathspec 'nope.txt' did not match any file(s) known to git"),
        "{err:?}"
    );
    let log = f
        .cmd_in(&f.work, &["log", "--format=%s"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&log.stdout),
        "first\n",
        "a commit was recorded anyway"
    );
}

/// A spec run from the top of the work tree is unaffected — there is no prefix
/// to join.
#[test]
fn a_spec_from_the_top_is_unchanged() {
    let f = Fixture::new("top");
    std::fs::write(f.work.join("sub/inner.txt"), "two\n").unwrap();
    let (_, err, code) = f.run_in(&f.work, &["commit", "-m", "second", "sub/inner.txt"]);
    assert_eq!((code, err.as_str()), (0, ""), "{err}");
    assert_eq!(f.changed(), "sub/inner.txt\n");
}
