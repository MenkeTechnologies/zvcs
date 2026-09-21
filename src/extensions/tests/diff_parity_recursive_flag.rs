//! `git diff -r`.
//!
//! `setup_revisions()` reads it as
//!
//! ```c
//! } else if (!strcmp(arg, "-r")) {
//!         revs->diff = 1;
//!         revs->diffopt.flags.recursive = 1;
//! ```
//! (revision.c:2551-2553), and `cmd_diff()` assigns
//! `rev.diffopt.flags.recursive = 1` unconditionally once parsing is done
//! (builtin/diff.c:542) while always rendering a diff. For this verb the flag
//! therefore changes nothing — but the port refused it as an unknown option,
//! which cost t4013-diff-various.sh five cases whose only distinguishing feature
//! was a `-r` in the argument list.
//!
//! `-t` is deliberately still refused. It is `recursive` *plus*
//! `tree_in_recursive` (revision.c:2554-2557), and that second bit does change
//! bytes: a tree-to-tree diff then lists each added or removed directory
//! alongside the blobs beneath it. Accepting it as a no-op would print the wrong
//! records rather than refuse, so the refusal stands until the tree walk can
//! emit those entries.
//!
//! Every expectation was measured from stock git 2.55.0 over the same fixture.
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
    /// Two commits: the second adds `d/e/g` and rewrites `f`, so a tree-to-tree
    /// diff has both a blob under a new directory and a plain modification.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-diff-rflag-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("f"), "x\n").unwrap();
        f.git(&["add", "f"]);
        f.git(&["commit", "-q", "-m", "one"]);
        std::fs::create_dir_all(f.work.join("d/e")).unwrap();
        std::fs::write(f.work.join("d/e/g"), "y\n").unwrap();
        std::fs::write(f.work.join("f"), "z\n").unwrap();
        f.git(&["add", "d", "f"]);
        f.git(&["commit", "-q", "-m", "two"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("ZVCS_HOME", self.root.join("zvcs"))
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
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn stdout(&self, args: &[&str]) -> String {
        let out = self.cmd(args).output().unwrap();
        assert_eq!(out.status.code(), Some(0), "`git {args:?}`: {out:?}");
        assert_eq!(String::from_utf8_lossy(&out.stderr), "", "`git {args:?}`");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

/// `-r` is accepted and changes nothing, wherever it sits in the argument list
/// and whatever output format it keeps company with.
#[test]
fn dash_r_is_accepted_and_changes_no_bytes() {
    let f = Fixture::new("noop");
    for format in [
        vec!["HEAD~1..HEAD"],
        vec!["--raw", "HEAD~1..HEAD"],
        vec!["--stat", "HEAD~1..HEAD"],
        vec!["--patch-with-raw", "HEAD~1..HEAD"],
        vec!["--patch-with-stat", "HEAD~1..HEAD"],
        vec!["--name-status", "HEAD~1..HEAD"],
    ] {
        let mut plain = vec!["diff"];
        plain.extend_from_slice(&format);
        let want = f.stdout(&plain);
        assert!(!want.is_empty(), "{format:?} produced nothing to compare");

        let mut leading = vec!["diff", "-r"];
        leading.extend_from_slice(&format);
        assert_eq!(f.stdout(&leading), want, "{format:?}");

        let mut trailing = vec!["diff"];
        trailing.extend_from_slice(&format);
        trailing.push("-r");
        assert_eq!(f.stdout(&trailing), want, "{format:?}");
    }
}

/// The blob under the newly added directory is listed by its full path and the
/// directory itself is not — which is what `recursive` without
/// `tree_in_recursive` means, and what `-r` leaves untouched.
#[test]
fn dash_r_lists_blobs_and_not_the_directories_above_them() {
    let f = Fixture::new("paths");
    let names = f.stdout(&["diff", "-r", "--name-only", "HEAD~1..HEAD"]);
    assert_eq!(names, "d/e/g\nf\n");
}

/// `-t` adds `tree_in_recursive`, so it is not the same flag and is still
/// refused rather than accepted as a no-op.
#[test]
fn dash_t_is_not_treated_as_a_synonym_for_dash_r() {
    let f = Fixture::new("tflag");
    let out = f.cmd(&["diff", "-t", "--raw", "HEAD~1..HEAD"]).output().unwrap();
    assert_ne!(out.status.code(), Some(0), "{out:?}");
    // Whatever the refusal says, it must not be the `-r` answer: a directory
    // record is the thing `-t` exists to add, and printing the `-r` listing under
    // `-t` would be silently wrong.
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("d/e/g"), "-t rendered the -r listing: {stdout}");
}
