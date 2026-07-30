//! `git stash`'s command-line surface: `save`'s options, `-h`, and the refusal
//! on an unmerged index.
//!
//! `save` is `push` with the message taken from the positional words
//! (`save_stash()` calls the same `do_push_stash()`), so `-u`/`-a`/`-k`/`-S`
//! work there exactly as they do for `push` — refusing them would make
//! `git stash save -u <msg>`, which scripts have used since long before `push`
//! existed, fail for no reason.
//!
//! `-h` is `parse_options()`' own: the usage block on **stdout**, exit 129, and
//! for the bare command that block lists every subcommand rather than `push`'s
//! options alone.
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
        let root = std::env::temp_dir().join(format!("zvcs-stashcli-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        std::fs::write(f.work.join("a.txt"), "base\n").unwrap();
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "base"]);
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

    /// `(exit code, stdout, stderr)`.
    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = self.cmd(args).output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn write(&self, path: &str, body: &str) {
        std::fs::write(self.work.join(path), body).unwrap();
    }
}

/// `save -u <message>` captures the untracked files and takes the message from
/// the positional words, like `push -u -m <message>` does.
#[test]
fn save_takes_the_same_options_as_push() {
    let f = Fixture::new("save-u");
    f.write("a.txt", "changed\n");
    f.write("new.txt", "untracked\n");

    let (code, out, err) = f.run(&["stash", "save", "-u", "my message"]);
    assert_eq!(code, 0, "save -u failed: {out}{err}");
    assert!(out.contains("On main: my message"), "message not used: {out}");
    assert!(!f.work.join("new.txt").exists(), "the untracked file was not captured");

    // `^3` is the untracked capture, which is what `-u` reached for.
    let (code, tree, err) = f.run(&["ls-tree", "-r", "--name-only", "stash@{0}^3"]);
    assert_eq!(code, 0, "no untracked parent: {tree}{err}");
    assert_eq!(tree.trim(), "new.txt");
}

/// `save -k` keeps the staged state staged, the same knob `push -k` has.
#[test]
fn save_keep_index_leaves_the_index_staged() {
    let f = Fixture::new("save-k");
    f.write("a.txt", "staged\n");
    f.git(&["add", "a.txt"]);

    let (code, out, err) = f.run(&["stash", "save", "-k", "keep"]);
    assert_eq!(code, 0, "save -k failed: {out}{err}");

    let (_, status, _) = f.run(&["status", "--porcelain"]);
    assert!(
        status.lines().any(|l| l == "M  a.txt"),
        "the staged change should still be staged: {status}"
    );
}

/// `-h` prints the subcommand's usage to stdout and exits 129 — the bare form
/// lists every subcommand, not `push`'s option table.
#[test]
fn dash_h_prints_usage_on_stdout() {
    let f = Fixture::new("dash-h");

    let (code, out, err) = f.run(&["stash", "-h"]);
    assert_eq!(code, 129, "wrong exit for `stash -h`");
    assert!(err.is_empty(), "usage must not go to stderr: {err}");
    assert!(out.starts_with("usage: git stash list"), "not the full usage block: {out}");
    for sub in ["show", "drop", "pop", "apply", "branch", "save", "clear", "create", "store"] {
        assert!(out.contains(&format!("git stash {sub}")), "`{sub}` missing from usage: {out}");
    }

    let (code, out, err) = f.run(&["stash", "pop", "-h"]);
    assert_eq!(code, 129, "wrong exit for `stash pop -h`");
    assert!(err.is_empty(), "usage must not go to stderr: {err}");
    assert!(out.starts_with("usage: git stash pop"), "not pop's usage: {out}");

    // An *unknown* option is the other half of `parse_options()`: the complaint
    // and the usage both go to stderr.
    let (code, out, err) = f.run(&["stash", "push", "--bogus"]);
    assert_eq!(code, 129, "wrong exit for an unknown option");
    assert!(out.is_empty(), "nothing belongs on stdout: {out}");
    assert!(err.starts_with("error: unknown option `bogus'"), "stderr: {err}");
}

/// A stash cannot be taken with unmerged entries in the index: git names each
/// conflicted path on stdout, then fails with `could not write index`.
#[test]
fn push_refuses_an_unmerged_index() {
    let f = Fixture::new("unmerged");
    f.git(&["checkout", "-q", "-b", "other"]);
    f.write("a.txt", "other\n");
    f.git(&["commit", "-q", "-am", "other"]);
    f.git(&["checkout", "-q", "main"]);
    f.write("a.txt", "main\n");
    f.git(&["commit", "-q", "-am", "main"]);
    // Expected to conflict, so the exit code is not asserted.
    let _ = f.run(&["merge", "other"]);

    let (code, out, err) = f.run(&["stash", "push", "-m", "x"]);
    assert_eq!(code, 1, "an unmerged index must fail the push: {out}{err}");
    assert_eq!(out, "a.txt: needs merge\n", "stdout: {out}");
    assert_eq!(err, "error: could not write index\n", "stderr: {err}");
}
