//! The prologue of `copy_or_rename_branch()` (builtin/branch.c:575-623), which
//! `-m`/`-M`/`-c`/`-C` share:
//!
//!   * `check_branch_ref()` on the *old* name comes first. A bad old name that
//!     exists anyway (`refs/heads/-x`, `refs/heads/HEAD`, made by `update-ref`)
//!     is a recovery: the operation proceeds and warns afterwards. One that does
//!     not exist dies with `invalid branch name: '<old>'` and the ref-syntax
//!     advice, before the new name is looked at.
//!   * The old-name messages print the operand as typed, so an `@{-1}` whose
//!     branch is gone is reported as `@{-1}`, not as what it expanded to.
//!   * The new name is validated only after the old one was found.
//!
//! Every expectation was measured from stock git in an identical throwaway
//! repository under the same pinned environment.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

const HINTS: &str = "hint: See 'git help check-ref-format'\n\
                     hint: Disable this message with \"git config set advice.refSyntax false\"\n";

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
    /// Two commits, so two tags can point at different objects.
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("zvcs-br-renameold-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "one\n").unwrap();
        f.git(&["add", "a"]);
        f.git(&["commit", "-q", "-m", "one"]);
        std::fs::write(f.work.join("a"), "two\n").unwrap();
        f.git(&["commit", "-q", "-am", "two"]);
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

/// A bad old name that does not exist is refused ahead of a bad new name.
#[test]
fn a_missing_misnamed_old_branch_is_an_invalid_branch_name() {
    let f = Fixture::new("invalid");

    for args in [
        &["branch", "-m", "", "a..b"][..],
        &["branch", "-c", "", "a..b"][..],
        &["branch", "-m", "--", "-nonexist", "zz"][..],
    ] {
        let (out, err, code) = f.run(args);
        let old = args[args.len() - 2];
        assert_eq!((out.as_str(), code), ("", 128), "{args:?}");
        assert_eq!(err, format!("fatal: invalid branch name: '{old}'\n{HINTS}"), "{args:?}");
    }
}

/// The old branch's existence is checked before the new name's syntax, and
/// reported with the operand as typed.
#[test]
fn a_missing_old_branch_is_named_as_typed() {
    let f = Fixture::new("missing");

    let (out, err, code) = f.run(&["branch", "-m", "nosuch", "a..b"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "fatal: no branch named 'nosuch'\n", 128));

    f.git(&["checkout", "-q", "-b", "side"]);
    f.git(&["checkout", "-q", "main"]);
    f.git(&["branch", "-D", "side"]);
    for flag in ["-m", "-c"] {
        let (out, err, code) = f.run(&["branch", flag, "@{-1}", "zz"]);
        assert_eq!(
            (out.as_str(), err.as_str(), code),
            ("", "fatal: no branch named '@{-1}'\n", 128),
            "{flag}"
        );
    }
}

/// A misnamed branch that exists is renamed or copied, with a warning.
#[test]
fn an_existing_misnamed_branch_is_recovered_with_a_warning() {
    let f = Fixture::new("recovery");
    f.git(&["update-ref", "refs/heads/-dash", "HEAD"]);
    f.git(&["update-ref", "refs/heads/HEAD", "HEAD~"]);

    let (out, err, code) = f.run(&["branch", "-m", "--", "-dash", "good"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "warning: renamed a misnamed branch '-dash' away\n", 0)
    );
    let (out, err, code) = f.run(&["branch", "-c", "HEAD", "hc"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "warning: created a copy of a misnamed branch 'HEAD'\n", 0)
    );
    assert_eq!(
        f.stdout(&["for-each-ref", "--format=%(refname)", "refs/heads/"]),
        "refs/heads/HEAD\nrefs/heads/good\nrefs/heads/hc\nrefs/heads/main\n"
    );
}
