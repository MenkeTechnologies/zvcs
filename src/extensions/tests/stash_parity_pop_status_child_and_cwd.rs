//! `git stash apply|pop` against stock git: the `git status` child, the merge
//! options read, the directory the process started in, and the order of
//! unmerged entries in `status --porcelain=v2`.
//!
//! * `do_apply_stash()` runs `git status` as a child and ignores its status, so a
//!   config value `git_status_config` refuses (`diff.relative=src/`, a negative
//!   `diff.context`) costs only the status output: `fatal:` on stderr, the stash is
//!   still dropped, exit 0.
//! * `init_ui_merge_options()` `die()`s on an unknown `diff.algorithm` before the
//!   index or worktree is touched: exit 128, the stash is kept.
//! * Popping from inside a directory the stash empties must not remove it: git
//!   never removes the directory the process started in, and the `../` path the
//!   worktree writer sees from there is the same directory.
//! * `status --porcelain=v2` prints the `1`/`2` entries sorted by path and then
//!   the `u` entries, not interleaved.
//!
//! Each case runs the same script under stock git and zvcs and compares stdout,
//! stderr, exit status and the finished worktree.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");
const DATE: &str = "1136214245 +0000";

struct Repo {
    root: PathBuf,
    work: PathBuf,
    bin: String,
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

struct Ran {
    stdout: String,
    stderr: String,
    code: Option<i32>,
}

impl Repo {
    fn new(tag: &str, bin: &str) -> Self {
        let label = if bin == BIN { "zvcs" } else { "stock" };
        let root = std::env::temp_dir()
            .join(format!("zvcs-stash-popchild-{tag}-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let root = root.canonicalize().unwrap();
        let work = root.join("work");
        let repo = Repo { root, work, bin: bin.to_owned() };
        repo.ok(&["init", "-q", "-b", "main", "."]);
        repo
    }

    fn command(&self, dir: &Path, args: &[&str]) -> Command {
        let mut c = Command::new(&self.bin);
        c.args(args)
            .current_dir(dir)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@e.co")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@e.co")
            .env("GIT_AUTHOR_DATE", DATE)
            .env("GIT_COMMITTER_DATE", DATE)
            .env("ZVCS_HOME", &self.root);
        c
    }

    fn run_in(&self, dir: &Path, envs: &[(&str, &str)], args: &[&str]) -> Ran {
        let mut c = self.command(dir, args);
        for (k, v) in envs {
            c.env(k, v);
        }
        let out = c.output().unwrap();
        Ran {
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            code: out.status.code(),
        }
    }

    fn run(&self, args: &[&str]) -> Ran {
        self.run_in(&self.work, &[], args)
    }

    fn ok(&self, args: &[&str]) {
        let r = self.run(args);
        assert_eq!(r.code, Some(0), "git {args:?}: {}", r.stderr);
    }

    fn write(&self, rel: &str, body: &str) {
        let p = self.work.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
}

fn assert_same(what: &str, stock: &Ran, zvcs: &Ran) {
    assert_eq!(stock.code, zvcs.code, "{what}: exit status");
    assert_eq!(stock.stdout, zvcs.stdout, "{what}: stdout");
    assert_eq!(stock.stderr, zvcs.stderr, "{what}: stderr");
}

fn both(test: impl Fn(&str)) {
    let Some(stock) = stock_git() else { return };
    test(stock);
    test(BIN);
}

/// `README.md` and `src/lib.rs` committed; then `README.md` edited, `src/lib.rs`
/// deleted and `staged.txt` added to the index, and all of it stashed.
fn stashed_with_deleted_src(tag: &str, bin: &str) -> Repo {
    let r = Repo::new(tag, bin);
    r.write("README.md", "one\n");
    r.write("src/lib.rs", "lib\n");
    r.ok(&["add", "."]);
    r.ok(&["commit", "-q", "-m", "base"]);
    r.write("README.md", "two\n");
    std::fs::remove_file(r.work.join("src/lib.rs")).unwrap();
    r.write("staged.txt", "s\n");
    r.ok(&["add", "staged.txt"]);
    r.ok(&["stash", "push", "-q", "-m", "gen"]);
    r
}

#[test]
fn pop_from_the_directory_it_empties_keeps_it() {
    let mut results: Vec<(Ran, bool, String)> = Vec::new();
    let Some(stock) = stock_git() else { return };
    for bin in [stock, BIN] {
        let r = stashed_with_deleted_src("cwd", bin);
        let src = r.work.join("src");
        assert!(src.is_dir(), "stash push restores src/lib.rs");
        let popped = r.run_in(&src, &[], &["stash", "pop"]);
        let kept = src.is_dir();
        let status = r.run(&["status", "--porcelain=v2"]).stdout;
        results.push((popped, kept, status));
    }
    let (stock, zvcs) = (&results[0], &results[1]);
    assert_same("stash pop from src/", &stock.0, &zvcs.0);
    assert!(stock.1, "stock keeps the starting directory");
    assert_eq!(stock.1, zvcs.1, "src/ survives the pop");
    assert_eq!(stock.2, zvcs.2, "status after the pop");
}

#[test]
fn unknown_diff_algorithm_dies_before_anything_is_applied() {
    let mut results: Vec<(Ran, String, String)> = Vec::new();
    let Some(stock) = stock_git() else { return };
    for bin in [stock, BIN] {
        let r = stashed_with_deleted_src("algo", bin);
        let popped = r.run(&["-c", "diff.algorithm=nonesuch", "stash", "pop"]);
        results.push((popped, r.run(&["stash", "list"]).stdout, r.run(&["status", "--porcelain"]).stdout));
    }
    let (stock, zvcs) = (&results[0], &results[1]);
    assert_eq!(stock.0.code, Some(128));
    assert_same("pop with a bad diff.algorithm", &stock.0, &zvcs.0);
    assert!(!stock.1.is_empty(), "the stash is kept");
    assert_eq!(stock.1, zvcs.1, "stash list");
    assert_eq!(stock.2, zvcs.2, "worktree untouched");
}

#[test]
fn status_child_refusing_config_does_not_fail_the_pop() {
    let mut results: Vec<(Ran, String, String)> = Vec::new();
    let Some(stock) = stock_git() else { return };
    for (key, value) in [("diff.relative", "src/"), ("diff.context", "-1")] {
        results.clear();
        for bin in [stock, BIN] {
            let r = stashed_with_deleted_src("statuschild", bin);
            let envs = [
                ("GIT_CONFIG_COUNT", "1"),
                ("GIT_CONFIG_KEY_0", key),
                ("GIT_CONFIG_VALUE_0", value),
            ];
            let popped = r.run_in(&r.work, &envs, &["stash", "pop"]);
            results.push((popped, r.run(&["stash", "list"]).stdout, r.run(&["status", "--porcelain"]).stdout));
        }
        let (stock, zvcs) = (&results[0], &results[1]);
        assert_eq!(stock.0.code, Some(0), "{key}: the child's failure is ignored");
        assert!(stock.0.stdout.starts_with("Dropped "), "{key}: no status output, then the drop");
        assert_same(key, &stock.0, &zvcs.0);
        assert_eq!(stock.1, zvcs.1, "{key}: stash list");
        assert_eq!(stock.2, zvcs.2, "{key}: worktree");
    }
}

/// `a` and `c` conflict; `b`, `d` and `e` come back staged.
fn conflicted_pop(bin: &str) -> Repo {
    let r = Repo::new("order", bin);
    for f in ["a", "b", "c", "d", "e"] {
        r.write(f, "1\n");
    }
    r.ok(&["add", "."]);
    r.ok(&["commit", "-q", "-m", "base"]);
    for f in ["a", "b", "c", "d", "e"] {
        r.write(f, "2\n");
    }
    r.ok(&["add", "b", "d", "e"]);
    r.ok(&["stash", "push", "-q"]);
    for f in ["a", "c"] {
        r.write(f, "3\n");
    }
    r.ok(&["commit", "-q", "-a", "-m", "moved"]);
    let popped = r.run(&["stash", "pop"]);
    assert_eq!(popped.code, Some(1), "{}", popped.stderr);
    r
}

#[test]
fn porcelain_v2_prints_unmerged_after_the_ordinary_entries() {
    both(|bin| {
        let r = conflicted_pop(bin);
        let v2 = r.run(&["status", "--porcelain=v2"]).stdout;
        let kinds: Vec<&str> = v2.lines().map(|l| &l[..1]).collect();
        assert_eq!(kinds, ["1", "1", "1", "u", "u"], "{bin}: {v2}");
    });
    let Some(stock) = stock_git() else { return };
    let (s, z) = (conflicted_pop(stock), conflicted_pop(BIN));
    for args in [
        &["status", "--porcelain=v2"][..],
        &["status", "--porcelain=v2", "-z"][..],
        &["status", "--porcelain=v2", "--branch"][..],
    ] {
        assert_same(&format!("{args:?}"), &s.run(args), &z.run(args));
    }
}
