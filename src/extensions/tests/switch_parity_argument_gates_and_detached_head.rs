//! `git switch` argument checks, detached `HEAD` and `--orphan`, against stock git 2.56.
//!
//! * `checkout_branch()` reaches `die_if_some_operation_in_progress()` only after
//!   `parse_branchname_arg()` and `die_expecting_a_branch()` (builtin/checkout.c:1727-1744),
//!   so a mid-merge `switch nosuch` is `invalid reference`, not `cannot switch branch while
//!   merging`.
//! * "Already on" compares two branch paths (builtin/checkout.c:1022); a detached `HEAD` has
//!   none, so `switch HEAD~1` there is `a branch is expected`.
//! * A `HEAD` already detached at the target is neither rewritten nor logged
//!   (refs/files-backend.c:2857-2866), so a bare `switch --detach` there writes no reflog line.
//! * `-` is `@{-1}` to `parse_branchname_arg()` (builtin/checkout.c:1495-1531): a previous
//!   branch since deleted is `invalid reference: @{-1}`.
//! * `--orphan` without `-f` is a two-way merge from `HEAD`'s tree to the empty one
//!   (builtin/checkout.c:842-851): an entry `HEAD` lacks stays staged and on disk.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git_at_least;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// One fixture directory per binary; stock git builds it, the binary under test runs the case.
struct Repo {
    root: PathBuf,
    work: PathBuf,
    stock: String,
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[derive(Debug, PartialEq)]
struct Ran {
    stdout: String,
    stderr: String,
    code: Option<i32>,
}

impl Repo {
    fn new(tag: &str, label: &str, stock: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-switch-gates-{tag}-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("work")).unwrap();
        let root = root.canonicalize().unwrap();
        let work = root.join("work");
        let repo = Repo { root, work, stock: stock.to_owned() };
        repo.git(&["init", "-q", "-b", "main", "."]);
        repo
    }

    fn exec(&self, bin: &str, dir: &Path, args: &[&str], input: Option<&str>) -> Ran {
        let mut child = Command::new(bin)
            .args(args)
            .current_dir(dir)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CEILING_DIRECTORIES", &self.root)
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("ZVCS_HOME", &self.root)
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if let Some(text) = input {
            use std::io::Write as _;
            child.stdin.take().unwrap().write_all(text.as_bytes()).unwrap();
        }
        let out = child.wait_with_output().unwrap();
        let root = self.root.to_string_lossy().into_owned();
        Ran {
            stdout: String::from_utf8_lossy(&out.stdout).replace(&root, "<root>"),
            stderr: String::from_utf8_lossy(&out.stderr).replace(&root, "<root>"),
            code: out.status.code(),
        }
    }

    /// Stock git, for building the fixture and reading the result back.
    fn git(&self, args: &[&str]) -> String {
        let r = self.exec(&self.stock, &self.work, args, None);
        assert_eq!(r.code, Some(0), "stock git {args:?}: {}", r.stderr);
        r.stdout
    }

    fn write(&self, rel: &str, body: &str) {
        let path = self.work.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    /// Refs, `HEAD`'s reflog, `ORIG_HEAD`, the index and the worktree as stock reads them.
    fn state(&self) -> Vec<String> {
        let mut files: Vec<String> = Vec::new();
        let mut stack = vec![self.work.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.file_name().is_some_and(|n| n == ".git") {
                    continue;
                }
                if path.is_dir() {
                    stack.push(path);
                } else {
                    let rel = path.strip_prefix(&self.work).unwrap().display().to_string();
                    files.push(format!("{rel}: {}", std::fs::read_to_string(&path).unwrap_or_default()));
                }
            }
        }
        files.sort();
        let orig_head = std::fs::read_to_string(self.work.join(".git/ORIG_HEAD")).unwrap_or_default();
        vec![
            self.git(&["for-each-ref", "--format=%(refname) %(objectname)"]),
            self.exec(&self.stock, &self.work, &["log", "-g", "--format=%H %gs", "HEAD"], None).stdout,
            orig_head,
            self.git(&["status", "--porcelain=v2", "--branch", "-uall"]),
            self.git(&["ls-files", "-s"]),
            files.join(""),
        ]
    }
}

/// Build the fixture once per binary with stock git, run `case` under each, and compare
/// what the case printed and what it left behind.
fn compare(tag: &str, setup: impl Fn(&Repo), case: impl Fn(&Repo, &str) -> Vec<Ran>) {
    let Some(stock) = stock_git_at_least((2, 56, 0)) else { return };
    let run = |label: &str, bin: &str| {
        let repo = Repo::new(tag, label, stock);
        setup(&repo);
        let ran = case(&repo, bin);
        (ran, repo.state())
    };
    assert_eq!(run("zvcs", BIN), run("stock", stock), "{tag}");
}

/// `main`: `a`, `b`, `dir/c`, `dir/sub/d` and `.gitignore`, then `a` and `b` edited in two
/// more commits; `side` branches off `main~1` and rewrites `b` and adds `s`; `v1` tags `main~1`.
fn base(r: &Repo) {
    r.write("a", "a1\na2\na3\na4\na5\n");
    r.write("b", "b1\n");
    r.write("dir/c", "c\n");
    r.write("dir/sub/d", "d\n");
    r.write(".gitignore", "*.log\n");
    r.git(&["add", "-A"]);
    r.git(&["commit", "-q", "-m", "one"]);
    r.write("a", "a1\na2\na3\na4\na5\na6\n");
    r.git(&["commit", "-q", "-am", "two"]);
    r.git(&["branch", "side"]);
    r.write("b", "b1\nb2\n");
    r.git(&["commit", "-q", "-am", "three"]);
    r.git(&["checkout", "-q", "side"]);
    r.write("b", "b-side\n");
    r.write("s", "s\n");
    r.git(&["add", "b", "s"]);
    r.git(&["commit", "-q", "-m", "side1"]);
    r.git(&["checkout", "-q", "main"]);
    r.git(&["tag", "v1", "HEAD~1"]);
}

fn switch(r: &Repo, bin: &str, args: &[&str]) -> Vec<Ran> {
    let mut argv = vec!["switch"];
    argv.extend_from_slice(args);
    vec![r.exec(bin, &r.work, &argv, None)]
}

fn merging(r: &Repo) {
    base(r);
    let ran = r.exec(&r.stock, &r.work, &["merge", "-q", "side"], None);
    assert_eq!(ran.code, Some(1), "{}", ran.stderr);
}

#[test]
fn argument_errors_come_before_the_operation_in_progress() {
    for args in [
        &["nosuch"][..],
        &["v1"],
        &["HEAD~1"],
        &["side", "--", "a"],
        &["-c", "main"],
        &["--orphan", "side"],
        &["--detach", "nosuch"],
        &["-"],
        // These pass every argument check and stop at the merge.
        &["side"],
        &["main"],
        &["-c", "fresh"],
        &["--detach", "HEAD~1"],
        &["--orphan", "fresh"],
    ] {
        compare("merging", merging, |r, bin| switch(r, bin, args));
    }
}

#[test]
fn a_detached_head_is_never_already_on_a_branch() {
    let detached = |r: &Repo| {
        base(r);
        r.git(&["checkout", "-q", "--detach", "HEAD~1"]);
        r.write("a", "dirty\n");
    };
    compare("det-rev", detached, |r, bin| switch(r, bin, &["HEAD~1"]));
    compare("det-head", detached, |r, bin| switch(r, bin, &["HEAD"]));
    // Already there: no reflog line.
    compare("det-bare", detached, |r, bin| switch(r, bin, &["--detach"]));
    compare("det-same", detached, |r, bin| switch(r, bin, &["--detach", "HEAD"]));
}

#[test]
fn a_deleted_previous_branch_is_an_invalid_reference() {
    let setup = |r: &Repo| {
        base(r);
        r.git(&["branch", "-q", "-D", "side"]);
    };
    compare("prev-dash", setup, |r, bin| switch(r, bin, &["-"]));
    compare("prev-at", setup, |r, bin| switch(r, bin, &["@{-1}"]));
}

#[test]
fn orphan_keeps_what_head_does_not_have() {
    let staged = |r: &Repo| {
        base(r);
        r.write("n", "new\n");
        r.git(&["add", "n"]);
    };
    compare("orphan-staged", staged, |r, bin| switch(r, bin, &["--orphan", "o"]));
    compare("orphan-force", staged, |r, bin| switch(r, bin, &["-f", "--orphan", "o"]));
    let unborn = |r: &Repo| {
        r.write("x", "x\n");
        r.write("y", "y\n");
        r.git(&["add", "x"]);
    };
    compare("orphan-unborn", unborn, |r, bin| switch(r, bin, &["--orphan", "o"]));
}
