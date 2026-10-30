//! `git stash` in a repository that has no index file, against stock git.
//!
//! * `do_push_stash()` reads the index with `repo_read_index_preload()`
//!   (builtin/stash.c:1704), and `do_read_index()` with `must_exist == 0` takes a
//!   missing file for an empty index (read-cache.c:2232). A pathspec then matches
//!   nothing, `repo_refresh_and_write_index()` creates the file (:1723), and an
//!   unborn `HEAD` is `You do not have the initial commit yet` — 128 for `push` and
//!   `save`, 1 for the assumed push.
//! * Over a `HEAD` whose index was deleted, every tracked file is untracked and
//!   staged-deleted at once: `--keep-index` skips its `checkout --no-overlay` for the
//!   empty `i_tree` (:1851-1866), `-u` runs `git clean` ahead of `reset --hard`
//!   (:1762-1780, 1819-1829), and `-S`'s `git apply -R` reports its patches last
//!   path first (apply.c:4918-4928).
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git_at_least;

const BIN: &str = env!("CARGO_BIN_EXE_git");
const DATE: &str = "1136214245 +0000";

struct Repo {
    root: PathBuf,
    work: PathBuf,
    bin: String,
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
    fn new(tag: &str, bin: &str, stock: &str) -> Self {
        let label = if bin == BIN { "zvcs" } else { "stock" };
        let root = std::env::temp_dir()
            .join(format!("zvcs-stash-missing-index-{tag}-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("work")).unwrap();
        let root = root.canonicalize().unwrap();
        let work = root.join("work");
        let repo = Repo { root, work, bin: bin.to_owned(), stock: stock.to_owned() };
        repo.ok(&["init", "-q", "-b", "main", "."]);
        repo
    }

    fn run_with(&self, bin: &str, args: &[&str]) -> Ran {
        let out = Command::new(bin)
            .args(args)
            .current_dir(&self.work)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("PWD", &self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CEILING_DIRECTORIES", &self.root)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@e.co")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@e.co")
            .env("GIT_AUTHOR_DATE", DATE)
            .env("GIT_COMMITTER_DATE", DATE)
            .env("ZVCS_HOME", &self.root)
            .output()
            .unwrap();
        let root = self.root.to_string_lossy().into_owned();
        Ran {
            stdout: String::from_utf8_lossy(&out.stdout).replace(&root, "<root>"),
            stderr: String::from_utf8_lossy(&out.stderr).replace(&root, "<root>"),
            code: out.status.code(),
        }
    }

    fn run(&self, args: &[&str]) -> Ran {
        self.run_with(&self.bin, args)
    }

    /// Fixture setup goes through stock git, so both sides start from the same bytes.
    fn ok(&self, args: &[&str]) {
        let r = self.run_with(&self.stock, args);
        assert_eq!(r.code, Some(0), "git {args:?}: {}", r.stderr);
    }

    fn write(&self, rel: &str, body: &str) {
        std::fs::write(self.work.join(rel), body).unwrap();
    }

    fn drop_index(&self) {
        std::fs::remove_file(self.work.join(".git/index")).unwrap();
    }

    /// What the command left behind, read by stock git: whether it wrote an index, the
    /// index itself, the worktree against it, every ref and the stash reflog.
    fn state(&self) -> (bool, String, String, String, String) {
        (
            self.work.join(".git/index").exists(),
            self.run_with(&self.stock, &["ls-files", "-s"]).stdout,
            self.run_with(&self.stock, &["status", "--porcelain"]).stdout,
            self.run_with(&self.stock, &["for-each-ref", "--format=%(refname) %(objectname)"]).stdout,
            self.run_with(&self.stock, &["stash", "list"]).stdout,
        )
    }
}

/// Run `case` under both binaries and compare what it returns.
fn compare<T: PartialEq + std::fmt::Debug>(case: impl Fn(&str, &str) -> T) {
    let Some(stock) = stock_git_at_least((2, 56, 0)) else { return };
    assert_eq!(case(BIN, stock), case(stock, stock));
}

const PUSHES: [&[&str]; 9] = [
    &["stash"],
    &["stash", "push"],
    &["stash", "save", "m"],
    &["stash", "push", "-q"],
    &["stash", "-k"],
    &["stash", "push", "-u"],
    &["stash", "push", "-a"],
    &["stash", "push", "-S"],
    &["stash", "push", "-m", "x", "--", "f"],
];

#[test]
fn an_unborn_repository_without_an_index_has_no_initial_commit() {
    for with_file in [false, true] {
        for args in PUSHES {
            compare(|bin, stock| {
                let r = Repo::new("unborn", bin, stock);
                if with_file {
                    r.write("f", "a\n");
                }
                (with_file, args, r.run(args), r.state())
            });
        }
    }
}

#[test]
fn a_deleted_index_under_head_is_an_empty_one() {
    for dirty in [false, true] {
        for args in PUSHES {
            compare(|bin, stock| {
                let r = Repo::new("head", bin, stock);
                r.write("f", "a\n");
                r.write("g", "g\n");
                r.ok(&["add", "f", "g"]);
                r.ok(&["commit", "-q", "-m", "c"]);
                r.drop_index();
                if dirty {
                    r.write("g", "b\n");
                    r.write("new", "n\n");
                }
                (dirty, args, r.run(args), r.state())
            });
        }
    }
}

#[test]
fn the_other_subcommands_read_a_missing_index_as_empty() {
    for args in [
        &["stash", "list"][..],
        &["stash", "show"],
        &["stash", "apply"],
        &["stash", "pop"],
        &["stash", "drop"],
        &["stash", "branch", "b"],
        &["stash", "create"],
        &["stash", "clear"],
    ] {
        compare(|bin, stock| {
            let r = Repo::new("stashed", bin, stock);
            r.write("f", "a\n");
            r.ok(&["add", "f"]);
            r.ok(&["commit", "-q", "-m", "c"]);
            r.write("f", "b\n");
            r.write("u", "u\n");
            r.ok(&["stash", "-q", "-u"]);
            r.drop_index();
            (args, r.run(args), r.state())
        });
    }
}
