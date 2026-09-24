//! A relative `core.worktree` that names no directory.
//!
//! `setup_explicit_git_dir()` installs a relative `core.worktree` by `chdir()`ing
//! to the git directory and then to the value (setup.c:1156-1170), so a value
//! naming nothing — the empty string included, which `chdir("")` refuses with
//! `ENOENT` — is `fatal: cannot chdir to '<value>': No such file or directory`
//! for every verb that runs setup, before the builtin looks at its arguments.
//! zvcs refused it only for `NEED_WORK_TREE` verbs, so `show-ref` answered and
//! `merge-recursive` printed its usage; the empty value failed repository
//! discovery outright.
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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-setup-core-wt-chdir-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file"), "one\n").unwrap();
        f.run(&["add", "file"]);
        f.run(&["commit", "-q", "-m", "init"]);
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

    fn append_config(&self, text: &str) {
        let path = self.work.join(".git/config");
        let mut config = std::fs::read_to_string(&path).unwrap();
        config.push_str(text);
        std::fs::write(path, config).unwrap();
    }
}

/// Per-worktree `core.worktree = no-such` (with `extensions.worktreeConfig`)
/// stops a read-only verb that never needs a work tree.
#[test]
fn missing_relative_worktree_stops_every_setup_verb() {
    let f = Fixture::new("nosuch");
    f.append_config("[extensions]\n\tworktreeConfig = true\n");
    std::fs::write(f.work.join(".git/config.worktree"), "[core]\n\tworktree = no-such\n").unwrap();
    let fatal = "fatal: cannot chdir to 'no-such': No such file or directory\n";
    for args in [&["show-ref", "--head"][..], &["status"], &["merge-recursive", "--", "main"]] {
        let (out, err, code) = f.run(args);
        assert_eq!((out.as_str(), err.as_str(), code), ("", fatal, 128), "{args:?}");
    }
    // `version` runs no setup at all.
    let (_, err, code) = f.run(&["version"]);
    assert_eq!((err.as_str(), code), ("", 0));
}

/// The empty value is a relative path `chdir()` refuses, not "no work tree".
#[test]
fn empty_worktree_is_a_failed_chdir() {
    let f = Fixture::new("empty");
    f.append_config("[core]\n\tworktree = \"\"\n");
    let fatal = "fatal: cannot chdir to '': No such file or directory\n";
    for args in [&["merge-recursive", "--", "main"][..], &["log", "--oneline"], &["rev-parse", "--show-toplevel"]] {
        let (out, err, code) = f.run(args);
        assert_eq!((out.as_str(), err.as_str(), code), ("", fatal, 128), "{args:?}");
    }
}

/// A relative value that does resolve still installs the work tree there.
#[test]
fn resolvable_relative_worktree_is_used() {
    let f = Fixture::new("dotdot");
    f.append_config("[core]\n\tworktree = ..\n");
    let (out, err, code) = f.run(&["rev-parse", "--show-toplevel"]);
    let top = std::fs::canonicalize(&f.work).unwrap();
    assert_eq!((out.as_str(), err.as_str(), code), (format!("{}\n", top.display()).as_str(), "", 0));
}
