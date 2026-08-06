//! `git status` announces a sparse checkout, and says how much of it is present.
//!
//! `wt_status_get_state()` computes the share only when `core.sparseCheckout` is on
//! and the index has entries; the number is `100 - (100 * skipped) / total` in
//! integer arithmetic over `skip-worktree` entries.
//! `show_sparse_checkout_in_use()` prints it as the last of the state blocks — after
//! any in-progress banner, before the initial-commit notice — followed by a blank
//! line.
//!
//! Expectations measured against stock git 2.55.0.
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
    /// Four committed files, of which `skip` are marked `skip-worktree` and removed
    /// from the worktree — the shape a sparse checkout leaves in a full index.
    fn new(tag: &str, skip: &[&str]) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-sparsebanner-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        for name in ["a.txt", "b.txt", "c.txt", "d.txt"] {
            std::fs::write(f.work.join(name), b"x\n").unwrap();
        }
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "init"]);
        if !skip.is_empty() {
            f.git(&["config", "core.sparseCheckout", "true"]);
            let mut args = vec!["update-index", "--skip-worktree"];
            args.extend_from_slice(skip);
            f.git(&args);
            for name in skip {
                std::fs::remove_file(f.work.join(name)).unwrap();
            }
        }
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    fn status(&self) -> String {
        let out = self.cmd(&["status"]).output().unwrap();
        assert!(out.status.success(), "`git status` failed: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

/// Half the tracked files present reads as 50%, and the banner sits between the
/// branch header and the rest.
#[test]
fn the_banner_reports_the_share_of_present_files() {
    let f = Fixture::new("half", &["b.txt", "c.txt"]);
    assert_eq!(
        f.status(),
        "On branch main\nYou are in a sparse checkout with 50% of tracked files present.\n\n\
         nothing to commit, working tree clean\n"
    );
}

/// The percentage is integer division, so one skipped file out of four is 75%.
#[test]
fn the_share_is_integer_arithmetic() {
    let f = Fixture::new("quarter", &["b.txt"]);
    assert!(
        f.status().contains("You are in a sparse checkout with 75% of tracked files present.\n"),
        "{}",
        f.status()
    );
}

/// Without `core.sparseCheckout` there is no banner at all, whatever the index says.
#[test]
fn a_plain_checkout_says_nothing() {
    let f = Fixture::new("plain", &[]);
    assert_eq!(f.status(), "On branch main\nnothing to commit, working tree clean\n");
}

/// The flag alone is not enough: git computes the share from `skip-worktree`
/// entries, so a fully present worktree reports 100%.
#[test]
fn a_fully_present_sparse_checkout_reports_full_presence() {
    let f = Fixture::new("full", &[]);
    f.git(&["config", "core.sparseCheckout", "true"]);
    assert!(
        f.status().contains("You are in a sparse checkout with 100% of tracked files present.\n"),
        "{}",
        f.status()
    );
}
