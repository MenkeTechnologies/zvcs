//! `git checkout <name> <path>…` when `<name>` is also a file on disk.
//!
//! ```c
//! if (!has_dash_dash) {	/* case (3).(d) -> (1) */
//!         /*
//!          * Do not complain the most common case
//!          *	git checkout branch
//!          * even if there happen to be a file called 'branch';
//!          * it would be extremely annoying.
//!          */
//!         if (argc)
//!                 verify_non_filename(prefix, arg);
//! }
//! ```
//!
//! (`parse_branchname_arg()`, builtin/checkout.c.) The exemption is for a lone
//! operand only. As soon as paths follow it, a leading operand that names both a
//! revision and a file is ambiguous, and `verify_non_filename()` (setup.c) dies
//! with the three-line `ambiguous argument` message. The port had no such check,
//! so `git checkout world all` quietly restored `all` out of the branch `world`.
//!
//! `check_filename()` is position-aware: it joins the operand onto the prefix,
//! so the same command run from a subdirectory that has no `world` in it is not
//! ambiguous and goes through — which is what t2010-checkout-ambiguous.sh's
//! case 8 pins down from both sides.
//!
//! Every expectation was measured from stock git 2.55.0 in an identical
//! throwaway repository under the same pinned environment.
#![cfg(unix)]

use std::path::{Path, PathBuf};
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
    /// Files `world` and `all`, plus a branch also called `world`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-idx-amb-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("world"), "hello\n").unwrap();
        std::fs::write(f.work.join("all"), "hello\n").unwrap();
        f.git(&["add", "all", "world"]);
        f.git(&["commit", "-q", "-m", "initial"]);
        f.git(&["branch", "world"]);
        f
    }

    fn cmd_in(&self, dir: &Path, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(dir)
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
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd_in(&self.work, args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn run_in(&self, dir: &Path, args: &[&str]) -> (String, i32) {
        let out = self.cmd_in(dir, args).output().unwrap();
        (
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn run(&self, args: &[&str]) -> (String, i32) {
        self.run_in(&self.work, args)
    }
}

const AMBIGUOUS: &str = "fatal: ambiguous argument 'world': both revision and filename\n\
Use '--' to separate paths from revisions, like this:\n\
'git <command> [<revision>...] -- [<file>...]'\n";

/// t2010-checkout-ambiguous.sh's case 7.
#[test]
fn index_parity_checkout_refuses_an_operand_that_is_both_a_ref_and_a_file() {
    let f = Fixture::new("both");
    let (err, code) = f.run(&["checkout", "world", "all"]);
    assert_eq!(code, 128, "the ambiguous checkout was accepted: {err}");
    assert_eq!(err, AMBIGUOUS);
}

/// The exemption the comment in `parse_branchname_arg()` is about: a lone
/// operand is a branch switch, file of the same name or not.
#[test]
fn index_parity_checkout_still_allows_the_lone_operand() {
    let f = Fixture::new("lone");
    let (err, code) = f.run(&["checkout", "world"]);
    assert_eq!(code, 0, "the lone operand was refused: {err}");
    assert_eq!(
        f.cmd_in(&f.work, &["symbolic-ref", "HEAD"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap(),
        "refs/heads/world"
    );

    // `--` says which side the operand is on, so neither spelling is ambiguous.
    let (err, code) = f.run(&["checkout", "world", "--", "all"]);
    assert_eq!(code, 0, "an explicit -- was refused: {err}");
}

/// t2010's case 8: `check_filename()` resolves the operand against the prefix,
/// so the same command is ambiguous only where the file actually is.
#[test]
fn index_parity_checkout_ambiguity_is_measured_from_the_current_directory() {
    let f = Fixture::new("subdir");
    let sub = f.work.join("sub");
    std::fs::create_dir(&sub).unwrap();

    // No `sub/world`, so nothing is ambiguous there.
    let (err, code) = f.run_in(&sub, &["checkout", "world", "../all"]);
    assert_eq!(code, 0, "a non-ambiguous subdirectory checkout failed: {err}");

    std::fs::write(sub.join("world"), "hello\n").unwrap();
    let (err, code) = f.run_in(&sub, &["checkout", "world", "../all"]);
    assert_eq!(code, 128, "the ambiguous checkout was accepted: {err}");
    assert_eq!(err, AMBIGUOUS);
}

/// An operand that is not a file is never ambiguous, however many paths follow.
#[test]
fn index_parity_checkout_accepts_a_ref_with_no_file_of_that_name() {
    let f = Fixture::new("noflie");
    f.git(&["branch", "other"]);
    std::fs::write(f.work.join("all"), "changed\n").unwrap();

    let (err, code) = f.run(&["checkout", "other", "all"]);
    assert_eq!(code, 0, "an unambiguous ref operand was refused: {err}");
    assert_eq!(std::fs::read_to_string(f.work.join("all")).unwrap(), "hello\n");
}
