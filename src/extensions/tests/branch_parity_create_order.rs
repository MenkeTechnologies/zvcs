//! `create_branch()` (branch.c:596-621) calls `validate_new_branchname()`
//! before `dwim_branch_start()`, so a name that already exists, or one a
//! worktree has checked out under `--force`, is refused before the
//! start-point is ever resolved. A bad start-point only surfaces once the
//! name itself is acceptable.
//!
//! Every expectation was measured from stock git in an identical throwaway
//! repository under the same pinned environment.
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
    /// Two commits, so two tags can point at different objects.
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("zvcs-br-createorder-{tag}-{}", std::process::id()));
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

/// An existing name is refused ahead of an unresolvable start-point.
#[test]
fn an_existing_name_is_refused_before_the_start_point_resolves() {
    let f = Fixture::new("exists");
    f.git(&["branch", "side", "HEAD~"]);

    for name in ["main", "side"] {
        let (out, err, code) = f.run(&["branch", name, "nosuch"]);
        assert_eq!((out.as_str(), code), ("", 128), "{name}");
        assert_eq!(err, format!("fatal: a branch named '{name}' already exists\n"));
    }
}

/// `--force` onto the checked-out branch is refused before the start-point
/// resolves; onto a branch nobody stands on, the start-point is what fails.
#[test]
fn a_forced_checked_out_branch_is_refused_before_the_start_point_resolves() {
    let f = Fixture::new("force");
    f.git(&["branch", "side", "HEAD~"]);
    let top = std::fs::canonicalize(&f.work).unwrap();

    let (out, err, code) = f.run(&["branch", "-f", "main", "nosuch"]);
    assert_eq!((out.as_str(), code), ("", 128));
    assert_eq!(
        err,
        format!(
            "fatal: cannot force update the branch 'main' used by worktree at '{}'\n",
            top.display()
        )
    );

    let (out, err, code) = f.run(&["branch", "-f", "side", "nosuch"]);
    assert_eq!((out.as_str(), code), ("", 128));
    assert_eq!(err, "fatal: not a valid object name: 'nosuch'\n");
    assert_eq!(f.stdout(&["rev-parse", "side"]), f.stdout(&["rev-parse", "HEAD~"]));
}
