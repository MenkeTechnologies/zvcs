//! `git diff-tree <commit>` prints its commit header only for a non-empty queue.
//!
//! `log_tree_diff_flush()` runs `diffcore_std()` first and returns before
//! `show_log()` when the queue came out empty (log-tree.c:929-940), so a
//! pickaxe or `--diff-filter` that drops every pair drops the commit-id line
//! (or the `-v` / `--pretty` block) with it. zvcs decided on the pre-diffcore
//! change list and printed the header over an empty diff. `--always`
//! (log-tree.c `log_tree_diff()`'s `opt->always` arm) still prints it.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::PathBuf;
use std::process::{Command, Stdio};

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
    /// `one` adds `f` and `g`; `two` edits `f`'s second line to `B` and `g` to `y`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-diff-tree-empty-queue-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."], "");
        std::fs::write(f.work.join("f"), "a\nb\n").unwrap();
        std::fs::write(f.work.join("g"), "x\n").unwrap();
        f.run(&["add", "."], "");
        f.run(&["commit", "-q", "-m", "one"], "");
        std::fs::write(f.work.join("f"), "a\nB\n").unwrap();
        std::fs::write(f.work.join("g"), "y\n").unwrap();
        f.run(&["commit", "-q", "-am", "two"], "");
        f
    }

    fn run(&self, args: &[&str], stdin: &str) -> (String, String, i32) {
        let mut child = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@x")
            .env("GIT_COMMITTER_NAME", "C")
            .env("GIT_COMMITTER_EMAIL", "c@x")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

const TWO: &str = "51d27249edf305a18dbf51042d985684b8ea1e92";
const ONE: &str = "7418bbfff17658b993f332656284b6afc71ebaeb";

#[test]
fn a_pickaxe_that_keeps_nothing_drops_the_header() {
    let f = Fixture::new("pickaxe");
    assert_eq!(f.run(&["rev-parse", "HEAD"], "").0.trim(), TWO);
    for args in [
        &["diff-tree", "-r", "--name-only", "-Gzzz", "HEAD"][..],
        &["diff-tree", "-Gzzz", "HEAD"],
        &["diff-tree", "-r", "-s", "-Gzzz", "HEAD"],
        &["diff-tree", "-r", "--stat", "-Szzz", "HEAD"],
        &["diff-tree", "-r", "-v", "-Gzzz", "HEAD"],
        &["diff-tree", "-r", "--diff-filter=D", "HEAD"],
    ] {
        assert_eq!(f.run(args, ""), (String::new(), String::new(), 0), "{args:?}");
    }
}

#[test]
fn a_pickaxe_that_keeps_a_pair_keeps_the_header() {
    let f = Fixture::new("keep");
    let (out, err, code) = f.run(&["diff-tree", "-r", "--name-only", "-GB", "HEAD"], "");
    assert_eq!((out.as_str(), err.as_str(), code), (format!("{TWO}\nf\n").as_str(), "", 0));
    let (out, _, code) = f.run(&["diff-tree", "-r", "-s", "-GB", "HEAD"], "");
    assert_eq!((out, code), (format!("{TWO}\n"), 0));
    let (out, _, code) = f.run(&["diff-tree", "-r", "--always", "-Gzzz", "HEAD"], "");
    assert_eq!((out, code), (format!("{TWO}\n"), 0));
}

#[test]
fn stdin_commits_each_decide_their_own_header() {
    let f = Fixture::new("stdin");
    let (out, err, code) =
        f.run(&["diff-tree", "--stdin", "-r", "--root", "--name-only", "-GB"], &format!("{TWO}\n{ONE}\n"));
    assert_eq!((out.as_str(), err.as_str(), code), (format!("{TWO}\nf\n").as_str(), "", 0));
    let (out, _, _) =
        f.run(&["diff-tree", "--stdin", "-r", "--root", "--name-only", "-Gx"], &format!("{TWO}\n{ONE}\n"));
    assert_eq!(out, format!("{TWO}\ng\n{ONE}\ng\n"));
}
