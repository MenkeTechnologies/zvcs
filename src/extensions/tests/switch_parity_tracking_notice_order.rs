//! The `set up to track` notice follows the local-changes listing on stdout.
//!
//! `switch_branches()` runs `merge_working_tree()`, whose tail prints
//! `show_local_changes()` (builtin/checkout.c:930-931), before
//! `update_refs_for_switch()` (builtin/checkout.c:1261) reaches
//! `create_branch()` → `setup_tracking()` and its `printf_ln()` notice
//! (branch.c:168-171). Both go to stdio's `stdout`, so they come out in that
//! order whatever fd 1 is.
//!
//! zvcs buffered the listing (as stdio does off a terminal) but wrote the
//! notice straight to fd 1, so a captured `switch -c`/`checkout -b` printed the
//! notice first.
//!
//! Expectations measured from stock git 2.56.0 under the same environment.

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
    /// One commit on `main`, `origin/main` at it, and `file` dirty in the worktree.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-switch-tracking-notice-order-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file"), "base\n").unwrap();
        f.run(&["add", "file"]);
        f.run(&["commit", "-q", "-m", "base"]);
        f.run(&["config", "remote.origin.url", "/nowhere"]);
        f.run(&["config", "remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"]);
        f.run(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        std::fs::write(f.work.join("file"), "base\nlocal edit\n").unwrap();
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
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

const LISTING_THEN_NOTICE: &str = "M\tfile\nbranch 'mine' set up to track 'origin/main'.\n";

#[test]
fn switch_create_prints_the_listing_before_the_tracking_notice() {
    let f = Fixture::new("switch");
    let got = f.run(&["switch", "-c", "mine", "origin/main"]);
    assert_eq!(
        (got.0.as_str(), got.1.as_str(), got.2),
        (LISTING_THEN_NOTICE, "Switched to a new branch 'mine'\n", 0)
    );
}

#[test]
fn switch_create_track_inherit_prints_the_listing_first() {
    let f = Fixture::new("inherit");
    f.run(&["branch", "--set-upstream-to=origin/main", "main"]);
    let got = f.run(&["switch", "-c", "mine", "--track=inherit", "main"]);
    assert_eq!(
        (got.0.as_str(), got.1.as_str(), got.2),
        (LISTING_THEN_NOTICE, "Switched to a new branch 'mine'\n", 0)
    );
}

#[test]
fn checkout_b_prints_the_listing_before_the_tracking_notice() {
    let f = Fixture::new("checkout");
    let got = f.run(&["checkout", "-b", "mine", "origin/main"]);
    assert_eq!(
        (got.0.as_str(), got.1.as_str(), got.2),
        (LISTING_THEN_NOTICE, "Switched to a new branch 'mine'\n", 0)
    );
}
