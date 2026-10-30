//! `checkout`, `switch`, `restore` and `reset` in a repository whose index file is missing,
//! against stock git.
//!
//! * `do_read_index()` with `must_exist == 0` takes a missing file for an empty index
//!   (read-cache.c:2232), so a pathspec over it matches nothing, a path reset stages the
//!   named paths alone, and `restore --staged` writes the file.
//! * `merge_working_tree()` sets `initial_checkout = is_index_unborn()` (builtin/checkout.c:829),
//!   and `twoway_merge()` then sends every new-tree path to `merged_entry()` even where both
//!   trees agree (unpack-trees.c:2977-2987): an untracked file in the way refuses the switch.
//! * `reset --merge`/`--keep` run `unpack_trees()` without `show_all_errors`, so only the first
//!   refused path is reported (unpack-trees.c:249-251), and `--keep` refuses an untracked file
//!   on a path the target drops (`deleted_entry()`, unpack-trees.c:2683-2686).
//! * A bare `git checkout -b <branch>` skips `merge_working_tree()` (builtin/checkout.c:1206-1207)
//!   and writes no index; `--staged` with no `--source` resolves `HEAD`, which dies on an unborn
//!   branch (builtin/checkout.c:1972-1973, 2026-2027).
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
            .join(format!("zvcs-checkout-missing-index-{tag}-{label}-{}", std::process::id()));
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

const COMMANDS: [&[&str]; 22] = [
    &["checkout"],
    &["checkout", "HEAD"],
    &["checkout", "-b", "b"],
    &["checkout", "-b", "b", "HEAD"],
    &["checkout", "--detach", "HEAD"],
    &["checkout", "HEAD", "--", "f"],
    &["checkout", "--", "f"],
    &["checkout", "side"],
    &["switch", "-c", "b"],
    &["switch", "-c", "b", "HEAD"],
    &["switch", "--detach", "HEAD"],
    &["switch", "side"],
    &["restore", "f"],
    &["restore", "--staged", "f"],
    &["restore", "-s", "HEAD", "f"],
    &["restore", "-S", "-W", "f"],
    &["reset", "--", "f"],
    &["reset", "HEAD", "--", "f"],
    &["reset", "-q"],
    &["reset", "--keep", "side"],
    &["reset", "--merge", "side"],
    &["status", "--porcelain"],
];

#[test]
fn a_deleted_index_under_head_is_an_empty_one() {
    // The worktree as the commit left it, with one file edited and one gone.
    for (edit, gone) in [(false, false), (true, false), (false, true)] {
        for args in COMMANDS {
            compare(|bin, stock| {
                let r = Repo::new("head", bin, stock);
                r.write("f", "a\n");
                r.write("g", "g\n");
                r.ok(&["add", "f", "g"]);
                r.ok(&["commit", "-q", "-m", "c"]);
                r.ok(&["branch", "side"]);
                r.write("f", "b\n");
                r.ok(&["commit", "-q", "-a", "-m", "d"]);
                r.drop_index();
                if edit {
                    r.write("g", "e\n");
                }
                if gone {
                    std::fs::remove_file(r.work.join("f")).unwrap();
                    std::fs::remove_file(r.work.join("g")).unwrap();
                }
                (edit, gone, args, r.run(args), r.state())
            });
        }
    }
}

#[test]
fn an_unborn_branch_has_no_head_to_restore_from() {
    for staged in [false, true] {
        for args in [&["restore", "--staged", "f"][..], &["restore", "-S", "-W", "f"], &["restore", "f"]] {
            compare(|bin, stock| {
                let r = Repo::new("unborn", bin, stock);
                r.write("f", "a\n");
                if staged {
                    r.ok(&["add", "f"]);
                }
                (staged, args, r.run(args), r.state())
            });
        }
    }
}

#[test]
fn a_two_tree_reset_reports_its_first_refusal_only() {
    for args in [&["reset", "--merge", "side"][..], &["reset", "--keep", "side"]] {
        // Both files edited on the branch and again in the worktree.
        compare(|bin, stock| {
            let r = Repo::new("stale", bin, stock);
            r.write("f", "a\n");
            r.write("g", "b\n");
            r.ok(&["add", "f", "g"]);
            r.ok(&["commit", "-q", "-m", "c"]);
            r.ok(&["branch", "side"]);
            r.write("f", "c\n");
            r.write("g", "d\n");
            r.ok(&["commit", "-q", "-a", "-m", "d"]);
            r.write("f", "e\n");
            r.write("g", "e\n");
            (args, r.run(args), r.state())
        });
        // Two files the target drops, unstaged but still on disk.
        compare(|bin, stock| {
            let r = Repo::new("untracked", bin, stock);
            r.write("f", "a\n");
            r.ok(&["add", "f"]);
            r.ok(&["commit", "-q", "-m", "c"]);
            r.ok(&["branch", "side"]);
            r.write("g", "x\n");
            r.write("h", "y\n");
            r.ok(&["add", "g", "h"]);
            r.ok(&["commit", "-q", "-m", "d"]);
            r.ok(&["rm", "-q", "--cached", "g", "h"]);
            (args, r.run(args), r.state())
        });
    }
}
