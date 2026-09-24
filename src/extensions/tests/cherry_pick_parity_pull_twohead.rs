//! `pull.twohead` is the sequencer's default strategy.
//!
//! `git_sequencer_config()` stores the first `pull.twohead` value, cut at its
//! first space, as `default_strategy` (sequencer.c:308-320), and
//! `run_sequencer()` adopts it when no `--strategy` was given
//! (builtin/revert.c:222-225). `do_pick_commit()` then runs every name other
//! than `ort`/`recursive` as a `git merge-<name>` child. zvcs ignored the key
//! and always merged in process.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.
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
    /// `side` adds `other` on top of `main`'s `base`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-cherry-pick-twohead-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file"), "base\n").unwrap();
        f.run(&["add", "file"]);
        f.run(&["commit", "-q", "-m", "base"]);
        f.run(&["checkout", "-q", "-b", "side"]);
        std::fs::write(f.work.join("other"), "side\n").unwrap();
        f.run(&["add", "other"]);
        f.run(&["commit", "-q", "-m", "side"]);
        f.run(&["checkout", "-q", "main"]);
        f
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
            .env("GIT_MERGE_AUTOEDIT", "no")
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

/// An unknown default strategy fails in its child, and the pick stops there.
#[test]
fn an_unknown_twohead_is_run_as_a_merge_child() {
    let f = Fixture::new("bogus");
    let (out, err, code) = f.run(&["-c", "pull.twohead=bogus", "cherry-pick", "side"]);
    assert_eq!((out.as_str(), code), ("", 1));
    assert!(
        err.starts_with("git: 'merge-bogus' is not a git command. See 'git --help'.\nerror: could not apply "),
        "{err:?}"
    );
    assert!(!f.work.join("other").exists());
    assert!(f.work.join(".git/CHERRY_PICK_HEAD").exists());
}

/// Only the first value counts, and only up to its first space; an explicit
/// `--strategy` wins over it.
#[test]
fn the_first_value_is_used_and_strategy_overrides_it() {
    let f = Fixture::new("first");
    let (out, _, code) =
        f.run(&["-c", "pull.twohead=resolve ort", "-c", "pull.twohead=bogus", "cherry-pick", "side"]);
    assert_eq!(code, 0);
    assert!(out.starts_with("Trying simple merge.\n[main "), "{out:?}");

    let f = Fixture::new("explicit");
    let (out, err, code) =
        f.run(&["-c", "pull.twohead=bogus", "cherry-pick", "--strategy=ort", "side"]);
    assert_eq!((err.as_str(), code), ("", 0), "{out}");
    assert_eq!(std::fs::read_to_string(f.work.join("other")).unwrap(), "side\n");
}
