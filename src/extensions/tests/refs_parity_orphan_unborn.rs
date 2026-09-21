//! `git checkout --orphan <name>` in a repository with no commits yet.
//!
//! `checkout_branch()` routes a new branch with no start-point commit to
//! `switch_unborn_to_new_branch()` when `HEAD` is a symref holding the null oid
//! (builtin/checkout.c:1740-1746 → :1551-1573, v2.55.0), and `--orphan` shares
//! `opts->new_branch` with `-b`/`-B` (:1960-1961), so all three spellings work
//! on a fresh `git init`. The port resolved the implicit `HEAD` start-point
//! instead and reported it as not a commit, which made
//! `git checkout --orphan main` unusable in an empty repository — the first
//! command of git's own t1400.
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
    /// An empty repository: `HEAD` is a symref at a branch that does not exist.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-orphan-unborn-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "trunk", "."]);
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
            .env("LC_ALL", "C")
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
}

/// The transition itself: `HEAD` is re-pointed, the branch stays unborn (no ref
/// is written), no reflog is opened for a `HEAD` that has no value to log a move
/// from, and the announcement goes to stderr.
#[test]
fn orphan_on_an_unborn_head_repoints_head_and_writes_no_ref() {
    let f = Fixture::new("basic");
    let (out, err, code) = f.run(&["checkout", "--orphan", "main"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "Switched to a new branch 'main'\n", 0)
    );

    let (head, _, _) = f.run(&["symbolic-ref", "HEAD"]);
    assert_eq!(head, "refs/heads/main\n");
    let (refs, _, code) = f.run(&["show-ref"]);
    assert_eq!((refs.as_str(), code), ("", 1), "the orphan branch is unborn");
    assert!(!f.work.join(".git/logs").exists(), "no reflog is created");
}

/// `--quiet` silences only the announcement; the transition still happens.
#[test]
fn orphan_on_an_unborn_head_is_silent_under_quiet() {
    let f = Fixture::new("quiet");
    let (out, err, code) = f.run(&["checkout", "-q", "--orphan", "main"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "", 0));
    let (head, _, _) = f.run(&["symbolic-ref", "HEAD"]);
    assert_eq!(head, "refs/heads/main\n");
}

/// `validate_new_branchname()` runs in `checkout_main()` before
/// `checkout_branch()` ever reaches the unborn path (builtin/checkout.c:2066-2074),
/// so an unusable name is still refused on an empty repository — the shortcut
/// must not skip the name check.
#[test]
fn orphan_on_an_unborn_head_still_refuses_an_invalid_name() {
    let f = Fixture::new("invalid");
    let (out, err, code) = f.run(&["checkout", "--orphan", "bad name"]);
    assert_eq!((out.as_str(), code), ("", 128));
    assert!(
        err.starts_with("fatal: 'bad name' is not a valid branch name"),
        "{err:?}"
    );
    let (head, _, _) = f.run(&["symbolic-ref", "HEAD"]);
    assert_eq!(head, "refs/heads/trunk\n", "HEAD is left alone");
}

/// An explicit start-point still has to resolve: with an operand present git
/// runs `parse_branchname_arg()` (builtin/checkout.c:1990-2000) and the unborn
/// shortcut does not apply.
#[test]
fn orphan_with_a_start_point_on_an_unborn_head_still_needs_it_to_resolve() {
    let f = Fixture::new("start");
    let (out, err, code) = f.run(&["checkout", "--orphan", "main", "nope"]);
    assert_eq!((out.as_str(), code), ("", 128));
    assert!(
        err.contains("is not a commit and a branch 'main' cannot be created from it"),
        "{err:?}"
    );
}
