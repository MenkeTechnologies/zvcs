//! `git switch` skipped `merge_working_tree()`'s opening gate.
//!
//! `switch` goes through the same `merge_working_tree()` as `checkout`, and that
//! starts with (builtin/checkout.c:883-889):
//!
//! ```c
//! refresh_index(the_repository->index, REFRESH_QUIET, NULL, NULL, NULL);
//!
//! if (unmerged_index(the_repository->index)) {
//!         rollback_lock_file(&lock_file);
//!         error(_("you need to resolve your current index first"));
//!         return 1;
//! }
//! ```
//!
//! zvcs's `switch` had neither half. With a conflicted index and no merge in
//! progress (a `stash pop` that conflicted) it answered with the two-way pass's
//! "would be overwritten" refusal, created the orphan branch anyway, and on the
//! current branch printed `Already on` with exit 0. Without the refresh, a file
//! whose mtime moved but whose content matches was listed as `M\t<path>` on
//! every switch.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::fs::File;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, SystemTime};

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
    /// `main` holds `a`; `side` adds `s` on top of it.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-switch-unmerged-gate-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "A"]);
        f.run(&["branch", "side"]);
        f.run(&["switch", "-q", "side"]);
        std::fs::write(f.work.join("s"), "s\n").unwrap();
        f.run(&["add", "s"]);
        f.run(&["commit", "-q", "-m", "S"]);
        f.run(&["switch", "-q", "main"]);
        f
    }

    /// Leave `a` conflicted with no merge in progress: a stash whose pop collides
    /// with a commit made after it.
    fn conflict(&self) {
        std::fs::write(self.work.join("a"), "a\nstashed\n").unwrap();
        self.run(&["stash", "-q"]);
        std::fs::write(self.work.join("a"), "a\ncommitted\n").unwrap();
        self.run(&["commit", "-q", "-am", "C"]);
        assert_eq!(self.run(&["stash", "pop"]).2, 1);
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
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
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

#[test]
fn every_switch_form_refuses_an_unmerged_index() {
    let f = Fixture::new("unmerged");
    f.conflict();
    let head = f.run(&["rev-parse", "HEAD"]).0;
    for args in [
        &["switch", "side"][..],
        &["switch", "main"],
        &["switch", "-c", "q", "side"],
        &["switch", "--detach", "side"],
        &["switch", "--orphan", "o"],
    ] {
        let (out, err, code) = f.run(args);
        assert_eq!(
            (out.as_str(), err.as_str(), code),
            ("a: needs merge\n", "error: you need to resolve your current index first\n", 1),
            "{args:?}"
        );
    }
    // Nothing moved and no branch was created.
    assert_eq!(f.run(&["rev-parse", "HEAD"]).0, head);
    assert_eq!(f.run(&["symbolic-ref", "HEAD"]).0, "refs/heads/main\n");
    assert_eq!(f.run(&["branch", "--list", "q", "o"]).0, "");
    // `-c` without a start point runs no `merge_working_tree()`, and `-f`
    // takes `reset_tree()` instead of the gate.
    assert_eq!(f.run(&["switch", "-c", "q2"]).2, 0);
    assert_eq!(f.run(&["switch", "-f", "side"]).2, 0);
    assert_eq!(f.run(&["status", "--porcelain"]).0, "");
}

#[test]
fn a_touched_file_is_not_a_local_change() {
    let f = Fixture::new("stat");
    let file = File::options().write(true).open(f.work.join("a")).unwrap();
    file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_500_000_000))
        .unwrap();
    assert_eq!(f.run(&["diff-files", "--name-only"]).0, "a\n");
    let (out, err, code) = f.run(&["switch", "side"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "Switched to branch 'side'\n", 0)
    );
    assert_eq!(f.run(&["diff-files", "--name-only"]).0, "");
}
