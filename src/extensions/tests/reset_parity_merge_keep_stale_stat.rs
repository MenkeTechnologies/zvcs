//! `git reset --merge` / `--keep` over an index whose stat data no longer matches the
//! worktree, against stock git 2.56.
//!
//! `reset_index()` reads the index with `repo_read_index_unmerged()` and refreshes nothing
//! (builtin/reset.c:62-118), so `verify_uptodate_1()` asks `ie_match_stat()` with only
//! `CE_MATCH_IGNORE_VALID|CE_MATCH_IGNORE_SKIP_WORKTREE` (unpack-trees.c:2224-2270): a file
//! whose mtime moved is `not uptodate` even when its bytes are still the entry's. The port
//! compared contents and reset over it.
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
        let root = std::env::temp_dir().join(format!("zvcs-reset-stale-stat-{tag}-{label}-{}", std::process::id()));
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

fn touch(r: &Repo, rel: &str) {
    let file = std::fs::File::options().write(true).open(r.work.join(rel)).unwrap();
    file.set_modified(std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_500_000_000))
        .unwrap();
}

fn reset(r: &Repo, bin: &str, args: &[&str]) -> Vec<Ran> {
    let mut argv = vec!["reset"];
    argv.extend_from_slice(args);
    vec![r.exec(bin, &r.work, &argv, None)]
}

#[test]
fn a_touched_staged_file_is_not_uptodate() {
    let setup = |r: &Repo| {
        base(r);
        r.write("b", "b1\nb2\nst\n");
        r.write("n", "new\n");
        r.git(&["add", "b", "n"]);
        touch(r, "b");
    };
    compare("merge-head", setup, |r, bin| reset(r, bin, &["--merge"]));
    compare("merge-prev", setup, |r, bin| reset(r, bin, &["--merge", "HEAD~1"]));
    compare("keep-side", setup, |r, bin| reset(r, bin, &["--keep", "side"]));
}

#[test]
fn the_first_stale_path_in_index_order_is_the_one_named() {
    // `b` is stale and `m` is intent-to-add; `unpack_trees()` stops at `b`.
    let setup = |r: &Repo| {
        base(r);
        r.write("m", "m\n");
        r.git(&["add", "-N", "m"]);
        touch(r, "b");
    };
    compare("ita-merge", setup, |r, bin| reset(r, bin, &["--merge", "HEAD~1"]));
    compare("ita-keep", setup, |r, bin| reset(r, bin, &["--keep", "HEAD~1"]));
}

#[test]
fn a_refreshed_index_resets() {
    let setup = |r: &Repo| {
        base(r);
        r.write("b", "b1\nb2\nst\n");
        r.git(&["add", "b"]);
        touch(r, "b");
        r.git(&["update-index", "-q", "--refresh"]);
    };
    compare("refreshed", setup, |r, bin| reset(r, bin, &["--merge", "HEAD~1"]));
}

#[test]
fn a_path_no_walked_tree_has_is_checked_after_the_walk() {
    // `0new` sorts first but only the index has it: `unpack_trees()` reaches it in the
    // left-over loop after the tree walk, so the stale `b` the walk meets is named.
    let setup = |r: &Repo| {
        base(r);
        r.write("0new", "z\n");
        r.git(&["add", "0new"]);
        touch(r, "0new");
        touch(r, "b");
    };
    compare("leftover-merge", setup, |r, bin| reset(r, bin, &["--merge", "HEAD~1"]));
    compare("leftover-keep", setup, |r, bin| reset(r, bin, &["--keep", "HEAD~1"]));
    // Only the left-over path is stale.
    let only = |r: &Repo| {
        base(r);
        r.write("0new", "z\n");
        r.git(&["add", "0new"]);
        touch(r, "0new");
    };
    compare("leftover-only", only, |r, bin| reset(r, bin, &["--merge", "HEAD~1"]));
}
