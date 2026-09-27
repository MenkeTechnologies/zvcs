//! The `git merge` child's config callback, which the in-process merge skipped.
//!
//! `run_merge()` (builtin/pull.c:521-570) runs `git merge` as a child process,
//! and that child starts with `repo_config(the_repository, git_merge_config,
//! …)` (builtin/merge.c:1400). A value `git_merge_config()` refuses —
//! `merge.autoStash`, `merge.verifySignatures`, `merge.branchdesc`, a valueless
//! `pull.octopus`, or `commit.gpgSign` through its `git_default_config` tail —
//! therefore ends the pull after the fetch, at the child's 128, which
//! `cmd_pull()` returns as its own status. zvcs merged in-process without the
//! dispatcher that runs that callback, and completed the pull at 0.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

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
    /// `side` is one commit ahead of `main`, so `pull . side` would fast-forward.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-pull-merge-child-config-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file"), "base\n").unwrap();
        f.run(&["add", "file"]);
        f.run(&["commit", "-q", "-m", "base"]);
        f.run(&["checkout", "-q", "-b", "side"]);
        std::fs::write(f.work.join("file"), "side\n").unwrap();
        f.run(&["commit", "-q", "-am", "side"]);
        f.run(&["checkout", "-q", "main"]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CEILING_DIRECTORIES", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("GIT_MERGE_AUTOEDIT", "no")
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn head(&self) -> String {
        self.run(&["rev-parse", "HEAD"]).0
    }
}

fn bad_bool(key: &str) -> String {
    format!("fatal: bad boolean config value 'bogus' for '{key}'\n")
}

#[test]
fn the_merge_callback_ends_the_pull_after_the_fetch() {
    let f = Fixture::new("keys");
    let before = f.head();
    for (key, lower) in [
        ("merge.autoStash", "merge.autostash"),
        ("commit.gpgSign", "commit.gpgsign"),
        ("merge.verifySignatures", "merge.verifysignatures"),
        ("merge.branchdesc", "merge.branchdesc"),
    ] {
        let (out, err, code) = f.run(&["-c", &format!("{key}=bogus"), "pull", ".", "side"]);
        let want = format!("From .\n * branch            side       -> FETCH_HEAD\n{}", bad_bool(lower));
        assert_eq!((out.as_str(), err.as_str(), code), ("", want.as_str(), 128), "{key}");
    }
    // The fetch ran; the merge did not.
    assert!(f.work.join(".git/FETCH_HEAD").exists());
    assert_eq!(f.head(), before);
}

/// A fast-forwardable `pull --rebase` runs the merge with `--ff-only`
/// (builtin/pull.c:1164-1169), so the merge child's callback runs there too.
#[test]
fn a_fast_forwarding_rebase_pull_runs_the_merge_child() {
    let f = Fixture::new("rebase");
    let (_, err, code) = f.run(&["-c", "merge.autoStash=bogus", "pull", "--rebase", ".", "side"]);
    assert_eq!((err.ends_with(&bad_bool("merge.autostash")), code), (true, 128), "{err}");
}

#[test]
fn a_valueless_octopus_strategy_is_a_nonbool_refusal() {
    let f = Fixture::new("octopus");
    let (_, err, code) = f.run(&["-c", "pull.octopus", "pull", ".", "side"]);
    assert!(
        err.ends_with(
            "error: missing value for 'pull.octopus'\n\
             fatal: unable to parse 'pull.octopus' from command-line config\n"
        ),
        "{err}"
    );
    assert_eq!(code, 128);
}

#[test]
fn a_valid_merge_configuration_still_merges() {
    let f = Fixture::new("valid");
    let (_, _, code) = f.run(&["-c", "merge.autoStash=true", "pull", "-q", ".", "side"]);
    assert_eq!(code, 0);
    assert_eq!(f.head(), f.run(&["rev-parse", "side"]).0);
}
