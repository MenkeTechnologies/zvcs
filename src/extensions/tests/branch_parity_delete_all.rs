//! `delete_branches()` (builtin/branch.c:237-258) chooses the namespace to
//! delete from by `filter.kind`, before it interprets any operand, and dies on
//! anything but local or remote-tracking: `-a` (both) is "cannot use -a with
//! -d". `cmd_branch()`'s "branch name required" (:859-860) is checked first,
//! and of `-a`/`-r` the later one on the command line wins.
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
            std::env::temp_dir().join(format!("zvcs-br-deleteall-{tag}-{}", std::process::id()));
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

/// `-a` with `-d`/`-D` refuses before touching, or even interpreting, a name.
#[test]
fn all_with_delete_is_refused() {
    let f = Fixture::new("refuse");
    f.git(&["branch", "side", "HEAD~"]);

    for args in [
        &["branch", "-D", "-a", "side"][..],
        &["branch", "-d", "-a", "nosuch@{u}"][..],
        &["branch", "-r", "-a", "-d", "side"][..],
    ] {
        let (out, err, code) = f.run(args);
        assert_eq!(
            (out.as_str(), err.as_str(), code),
            ("", "fatal: cannot use -a with -d\n", 128),
            "{args:?}"
        );
    }
    assert_eq!(f.stdout(&["branch"]), "* main\n  side\n");

    let (out, err, code) = f.run(&["branch", "-d", "-a"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "fatal: branch name required\n", 128));

    // `-a` then `-r`: the kind is remote-tracking again.
    let (out, err, code) = f.run(&["branch", "-a", "-r", "-d", "side"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "error: remote-tracking branch 'side' not found\n", 1)
    );
}
