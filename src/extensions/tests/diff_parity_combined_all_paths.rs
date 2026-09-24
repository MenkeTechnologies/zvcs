//! `git diff --combined-all-paths` without a combined mode.
//!
//! ```c
//! } else if (!strcmp(arg, "--combined-all-paths")) {
//!         revs->combined_all_paths = 1;
//! ```
//! (diff-merges.c:142-143), and once the scan is over
//!
//! ```c
//! if (revs->combined_all_paths && !revs->combine_merges)
//!         die("--combined-all-paths makes no sense without -c or --cc");
//! ```
//! (diff-merges.c:184-185), from `setup_revisions()` just ahead of
//! `diff_setup_done()` (revision.c:3170-3174). Every mode setter starts from
//! `suppress()`, which clears the flag again, so only a combined mode given
//! *before* it keeps it. The port did not know the option: `error: invalid
//! option` and the usage block at 129.
//!
//! Every expectation was measured from stock git 2.55.0 in an identical
//! throwaway repository.
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
    /// One commit on `main`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-diff-cap-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("path"), "a\n").unwrap();
        f.git(&["add", "path"]);
        f.git(&["commit", "-q", "-m", "one"]);
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
}

const NO_SENSE: &str = "fatal: --combined-all-paths makes no sense without -c or --cc\n";

/// Alone, after a positional, and ahead of an unknown option and a pickaxe
/// conflict: the die closes the scan and outranks both.
#[test]
fn without_a_combined_mode_it_dies_after_the_scan() {
    let f = Fixture::new("alone");
    for args in [
        &["diff", "--combined-all-paths"][..],
        &["diff", "--numstat", "--combined-all-paths", "path"][..],
        &["diff", "--combined-all-paths", "--bogus"][..],
        &["diff", "--combined-all-paths", "-S", "x", "-G", "y"][..],
    ] {
        assert_eq!(f.run(args), (String::new(), NO_SENSE.to_string(), 128), "{args:?}");
    }
}

/// A combined mode ahead of it keeps the flag and the diff runs. Any mode set
/// *after* it runs `suppress()` first, which drops the flag — so neither a later
/// `--no-diff-merges` nor a later `--diff-merges=c` leaves anything to refuse.
#[test]
fn after_a_combined_mode_it_is_accepted() {
    let f = Fixture::new("cc");
    for args in [
        &["diff", "--cc", "--combined-all-paths"][..],
        &["diff", "--diff-merges=dense-combined", "--combined-all-paths"][..],
        &["diff", "--cc", "--combined-all-paths", "--no-diff-merges"][..],
        &["diff", "--combined-all-paths", "--diff-merges=c"][..],
    ] {
        assert_eq!(f.run(args), (String::new(), String::new(), 0), "{args:?}");
    }
}
