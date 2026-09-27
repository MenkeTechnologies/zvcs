//! A relative local remote is read from the top of the work tree, not from the
//! directory the command was typed in.
//!
//! `setup_git_directory()` `chdir()`s to the top of the work tree for a command
//! started below it — `setup_discovered_git_dir()` (setup.c:1014-1015), and
//! `setup_explicit_git_dir()` for `--work-tree` (setup.c:960-971) — before
//! `cmd_push()`/`cmd_fetch()`/`cmd_ls_remote()` run. `git_connect()` then starts
//! the local `git-receive-pack '../up.git'`/`git-upload-pack` with `conn->dir`
//! unset (connect.c:1479-1491), so the service inherits that directory and the
//! path `remote.o.url = ../up.git` names a repository beside the work tree.
//!
//! zvcs never moves, and both its own "is this a repository" check and the
//! spawned service resolved `../up.git` from `w/sub`: every push, fetch and
//! ls-remote from a subdirectory died with "'../up.git' does not appear to be a
//! git repository".
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
    /// A bare `up.git` holding `main`, and a work repository `w` with a
    /// subdirectory `sub`, whose remote `o` is `../up.git`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-push-relurl-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("w");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run_in(&f.root, &["init", "-q", "--bare", "up.git"]);
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "a"]);
        f.run(&["remote", "add", "o", "../up.git"]);
        f.run(&["push", "-q", "o", "main"]);
        std::fs::create_dir(f.work.join("sub")).unwrap();
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        self.run_in(&self.work, args)
    }

    fn run_in(&self, dir: &std::path::Path, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(dir)
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

    fn in_sub(&self, args: &[&str]) -> (String, String, i32) {
        self.run_in(&self.work.join("sub"), args)
    }

    fn remote_refs(&self) -> String {
        self.run(&["--git-dir=../up.git", "for-each-ref", "--format=%(refname)"]).0
    }
}

#[test]
fn push_from_a_subdirectory_reaches_the_remote_beside_the_work_tree() {
    let f = Fixture::new("push");
    let (out, err, code) = f.in_sub(&["push", "o", "main:x"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "To ../up.git\n * [new branch]      main -> x\n", 0)
    );
    // `--work-tree=..` from the same place: setup moves to `..` just the same.
    let (_, err, code) = f.in_sub(&["--work-tree=..", "push", "o", "main:y"]);
    assert_eq!((err.as_str(), code), ("To ../up.git\n * [new branch]      main -> y\n", 0));
    // A path typed on the command line is read from there too.
    let (_, err, code) = f.in_sub(&["push", "../up.git", "main:z"]);
    assert_eq!((err.as_str(), code), ("To ../up.git\n * [new branch]      main -> z\n", 0));
    assert_eq!(
        f.remote_refs(),
        "refs/heads/main\nrefs/heads/x\nrefs/heads/y\nrefs/heads/z\n"
    );
}

#[test]
fn fetch_and_ls_remote_from_a_subdirectory() {
    let f = Fixture::new("fetch");
    let (out, err, code) = f.in_sub(&["ls-remote", "o"]);
    let head = f.run(&["rev-parse", "main"]).0;
    assert_eq!(
        (out, err.as_str(), code),
        (format!("{}\trefs/heads/main\n", head.trim_end()), "", 0)
    );
    let (out, err, code) = f.in_sub(&["fetch", "../up.git", "main"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "From ../up\n * branch            main       -> FETCH_HEAD\n", 0)
    );
    let (_, _, code) = f.in_sub(&["fetch", "o"]);
    assert_eq!(code, 0);
    assert!(f.run(&["rev-parse", "--verify", "-q", "refs/remotes/o/main"]).2 == 0);
}

#[test]
fn a_missing_repository_is_still_named_as_typed() {
    let f = Fixture::new("missing");
    let (out, err, code) = f.in_sub(&["push", "../nope.git", "main"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        (
            "",
            "fatal: '../nope.git' does not appear to be a git repository\n\
             fatal: Could not read from remote repository.\n\n\
             Please make sure you have the correct access rights\n\
             and the repository exists.\n",
            128
        )
    );
}
