//! What `git branch` does on the *filesystem* when a name is created, renamed
//! or copied — the parts `files_copy_or_rename_ref()`
//! (refs/files-backend.c:1635-1751) pins down and a straight
//! "write the new ref, then delete the old" does not:
//!
//!   * The old ref and its reflog are moved out of the way *first*, so
//!     `git branch -m m m/m` can turn the file `refs/heads/m` into a directory.
//!   * `remove_empty_directories()` (:1712-1723, :1184-1192) prunes the empty
//!     tree a deleted `refs/heads/n/n` leaves behind, so `git branch -m n/n n`
//!     can create the file `refs/heads/n`.
//!   * A symbolic ref is refused outright (:1668-1676), and
//!     `copy_or_rename_branch()` turns that into `branch rename failed`
//!     (builtin/branch.c:640-644) with both names untouched.
//!   * `--create-reflog` is `REF_FORCE_CREATE_REFLOG` (branch.c:625-626), so
//!     the log is written whatever `core.logAllRefUpdates` says.
//!   * git writes `.git/config` through a lock it holds on a path that need not
//!     exist, so a repository with no config file still takes a rename.
//!
//! Every expectation was measured from stock git 2.55.0 in an identical
//! throwaway repository under the same pinned environment.
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
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("zvcs-br-renamefs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.git(&["add", "a"]);
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

    fn stdout(&self, args: &[&str]) -> String {
        let (out, err, code) = self.run(args);
        assert_eq!((err.as_str(), code), ("", 0), "`git {args:?}`");
        out
    }

    fn has_reflog(&self, name: &str) -> bool {
        self.run(&["reflog", "exists", name]).2 == 0
    }
}

/// `git branch -m m m/m` turns a ref file into a directory, and
/// `git branch -m n/n n` turns the directory back into a file. Both carry the
/// reflog across.
#[test]
fn a_rename_across_a_directory_file_boundary_works_both_ways() {
    let f = Fixture::new("df");
    f.git(&["branch", "--create-reflog", "m"]);
    f.git(&["branch", "-m", "m", "m/m"]);
    assert!(f.has_reflog("refs/heads/m/m"));
    assert_eq!(f.stdout(&["rev-parse", "--verify", "refs/heads/m/m"]).len(), 41);

    f.git(&["branch", "--create-reflog", "n/n"]);
    f.git(&["branch", "-m", "n/n", "n"]);
    assert!(f.has_reflog("refs/heads/n"));
    assert_eq!(f.stdout(&["rev-parse", "--verify", "refs/heads/n"]).len(), 41);
}

/// Renaming the *checked-out* branch into a directory under its own name keeps
/// `HEAD` pointing at it.
#[test]
fn renaming_the_current_branch_into_its_own_subdirectory_repoints_head() {
    let f = Fixture::new("dfhead");
    f.git(&["checkout", "-q", "-b", "foo"]);
    f.git(&["branch", "-m", "foo/bar"]);
    assert_eq!(f.stdout(&["symbolic-ref", "HEAD"]).trim(), "refs/heads/foo/bar");
}

/// A sibling deleted out of `refs/heads/s/` leaves an empty directory where the
/// rename destination `refs/heads/s` has to become a file.
#[test]
fn an_emptied_ref_directory_does_not_block_the_destination() {
    let f = Fixture::new("emptydir");
    f.git(&["branch", "--create-reflog", "s/s"]);
    f.git(&["branch", "--create-reflog", "s/t"]);
    f.git(&["branch", "-d", "s/t"]);
    f.git(&["branch", "-m", "s/s", "s"]);
    assert!(f.has_reflog("refs/heads/s"));
}

/// `--create-reflog` writes the log even with `core.logAllRefUpdates` off.
#[test]
fn create_reflog_forces_the_log_past_log_all_ref_updates() {
    let f = Fixture::new("forcelog");
    let (out, err, code) = f.run(&[
        "-c",
        "core.logallrefupdates=false",
        "branch",
        "--create-reflog",
        "d/e/f",
    ]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "", 0));
    let head = f.stdout(&["rev-parse", "HEAD"]).trim().to_string();
    assert_eq!(
        f.stdout(&["reflog", "show", "--no-abbrev-commit", "refs/heads/d/e/f"]),
        format!("{head} refs/heads/d/e/f@{{0}}: branch: Created from main\n")
    );

    // Without the flag, the same configuration writes no log at all.
    f.git(&["-c", "core.logallrefupdates=false", "branch", "plain"]);
    assert!(!f.has_reflog("refs/heads/plain"));
}

/// A symbolic ref under `refs/heads/` is not renameable, and the failure leaves
/// the symref and its target where they were.
#[test]
fn renaming_a_symbolic_ref_is_refused() {
    let f = Fixture::new("symref");
    f.git(&["symbolic-ref", "refs/heads/topic", "refs/heads/main"]);

    let (out, err, code) = f.run(&["branch", "-m", "topic", "new-topic"]);
    assert_eq!((out.as_str(), code), ("", 128));
    assert_eq!(
        err,
        "error: refname refs/heads/topic is a symbolic ref, renaming it is not supported\n\
         fatal: branch rename failed\n"
    );
    assert_eq!(f.stdout(&["symbolic-ref", "refs/heads/topic"]).trim(), "refs/heads/main");
    assert_eq!(f.run(&["rev-parse", "--verify", "--quiet", "refs/heads/new-topic"]).2, 1);
}

/// A repository whose `.git/config` has been moved aside still renames, and the
/// rename writes the file back.
#[test]
fn a_missing_config_file_does_not_block_a_rename() {
    let f = Fixture::new("noconfig");
    f.git(&["branch", "q"]);
    let cfg = f.work.join(".git/config");
    let saved = f.work.join(".git/config-saved");
    std::fs::rename(&cfg, &saved).unwrap();

    let (out, err, code) = f.run(&["branch", "-m", "q", "q2"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "", 0));

    std::fs::rename(&saved, &cfg).unwrap();
    assert_eq!(f.stdout(&["rev-parse", "--verify", "refs/heads/q2"]).len(), 41);
    assert_eq!(f.run(&["rev-parse", "--verify", "--quiet", "refs/heads/q"]).2, 1);
}
