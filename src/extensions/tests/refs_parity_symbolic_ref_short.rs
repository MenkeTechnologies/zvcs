//! `git symbolic-ref --short`.
//!
//! `cmd_symbolic_ref()` prints `refs_shorten_unambiguous_ref(refs, refname, 0)`
//! (builtin/symbolic-ref.c:23-26, v2.55.0), whose rule matcher checks both ends
//! of the rule:
//!
//! ```c
//! /* And now check that our suffix (if any) matches. */
//! if (!strip_suffix(refname, rule, len))
//!         return NULL;
//! ```
//! (`match_parse_rule()`, refs.c:1616-1623). The last rev-parse rule is
//! `refs/remotes/%.*s/HEAD`, so `refs/remotes/origin/HEAD` shortens all the way
//! to `origin`. The port carried a second, prefix-only copy of the shortener in
//! `symbolic-ref` that answered `origin/HEAD`; it now calls the shared port.
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
    /// One commit on `main`, plus a remote-tracking `origin/side`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-symref-short-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.git(&["add", "a"]);
        f.git(&["commit", "-q", "-m", "subject"]);
        f.git(&["update-ref", "refs/remotes/origin/side", "HEAD"]);
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

    fn stdout(&self, args: &[&str]) -> String {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

/// The `refs/remotes/%.*s/HEAD` rule: `origin`, not `origin/HEAD`. The symref
/// names a `refs/remotes/origin/HEAD` that does not exist, so resolution stops
/// on that name and it is the one being shortened.
#[test]
fn a_remote_head_shortens_past_the_head_component() {
    let f = Fixture::new("remote-head");
    f.git(&["symbolic-ref", "TEST_SYMREF", "refs/remotes/origin/HEAD"]);
    assert_eq!(f.stdout(&["symbolic-ref", "--short", "TEST_SYMREF"]), "origin\n");
}

/// A remote-tracking branch that is not `<remote>/HEAD` keeps both components:
/// the suffix rule does not match it, and the `refs/remotes/%.*s` rule does.
#[test]
fn an_ordinary_remote_branch_keeps_its_remote_name() {
    let f = Fixture::new("remote-branch");
    f.git(&["symbolic-ref", "TEST_SYMREF", "refs/remotes/origin/side"]);
    assert_eq!(
        f.stdout(&["symbolic-ref", "--short", "TEST_SYMREF"]),
        "origin/side\n"
    );
}

/// The ordinary case is unchanged: a branch shortens to its own name, and
/// `--short` on `HEAD` names the branch it points at.
#[test]
fn a_branch_shortens_to_its_own_name() {
    let f = Fixture::new("branch");
    assert_eq!(f.stdout(&["symbolic-ref", "--short", "HEAD"]), "main\n");
    f.git(&["symbolic-ref", "TEST_SYMREF", "refs/heads/main"]);
    assert_eq!(f.stdout(&["symbolic-ref", "--short", "TEST_SYMREF"]), "main\n");
}

/// A shortening that would become ambiguous is refused: with a tag of the same
/// name in the way, the branch keeps a longer spelling.
#[test]
fn an_ambiguous_shortening_keeps_more_of_the_name() {
    let f = Fixture::new("ambiguous");
    f.git(&["tag", "dup"]);
    f.git(&["update-ref", "refs/heads/dup", "HEAD"]);
    f.git(&["symbolic-ref", "TEST_SYMREF", "refs/heads/dup"]);
    assert_eq!(
        f.stdout(&["symbolic-ref", "--short", "TEST_SYMREF"]),
        "heads/dup\n"
    );
}
