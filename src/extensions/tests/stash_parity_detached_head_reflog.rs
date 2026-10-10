//! `git stash push` logs `reset: moving to HEAD` only for an attached `HEAD`.
//!
//! The reset that clears the worktree rewrites `HEAD` with the value it already has. Through a
//! branch that still appends a `HEAD` reflog entry; a detached `HEAD` is a plain ref, and the
//! unchanged update writes nothing. zvcs logged the entry in both states. Expectations measured
//! from stock git 2.56.0.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str, detach: bool) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-stash-reflog-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.root.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "one"]);
        if detach {
            f.run(&["checkout", "-q", "--detach", "HEAD"]);
        }
        std::fs::write(f.root.join("a"), "changed\n").unwrap();
        f
    }

    fn run(&self, args: &[&str]) -> String {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn head_reflog_subjects(&self) -> Vec<String> {
        self.run(&["reflog", "show", "--format=%gs", "HEAD"]).lines().map(str::to_owned).collect()
    }
}

#[test]
fn an_attached_head_gets_the_entry() {
    let f = Fixture::new("attached", false);
    f.run(&["stash", "push", "-m", "wip"]);
    assert_eq!(f.head_reflog_subjects()[0], "reset: moving to HEAD");
}

#[test]
fn a_detached_head_does_not() {
    let f = Fixture::new("detached", true);
    f.run(&["stash", "push", "-m", "wip"]);
    assert_eq!(f.head_reflog_subjects()[0], "checkout: moving from main to HEAD");
}
