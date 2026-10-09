//! An `--attr-source` (or `GIT_ATTR_SOURCE`) that names nothing is fatal at the
//! first attribute lookup (`compute_default_attr_source()`, attr.c:1201-1228).
//! merge-ort makes that lookup from `ll_merge()`, which `handle_content_merge()`
//! calls for every blob both sides changed — so the merge dies
//! `fatal: bad --attr-source or GIT_ATTR_SOURCE` at 128 while it is still
//! collecting, ahead of the trees and commits it would write. A merge with no
//! content to merge never gets there.
//!
//! zvcs resolved the merge, wrote the commits and exited 0.

use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

#[derive(Debug, PartialEq, Eq)]
struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn git(bin: &str, dir: &Path, args: &[&str]) -> Run {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir.parent().unwrap())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("GIT_EDITOR", "true")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    Run {
        code: out.status.code().expect("no signal"),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn commit_file(bin: &str, dir: &Path, name: &str, body: &str, msg: &str) {
    std::fs::write(dir.join(name), body).unwrap();
    git(bin, dir, &["add", name]);
    git(bin, dir, &["commit", "-q", "-m", msg]);
}

/// `main` and `side` both change `shared`, on lines apart (a clean content merge), and each adds a file of its own.
/// `main` is checked out.
fn fixture(bin: &str, label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-badattr-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join("repo");
    std::fs::create_dir_all(&dir).unwrap();
    let dir = std::fs::canonicalize(&dir).unwrap();
    git(bin, &dir, &["init", "-q", "-b", "main"]);
    commit_file(bin, &dir, "shared", "1\n2\n3\n4\n5\n6\n7\n8\n9\n", "base");
    git(bin, &dir, &["checkout", "-q", "-b", "side"]);
    commit_file(bin, &dir, "shared", "1\n2\n3\n4\n5\n6\n7\n8\nnine\n", "side edits shared");
    commit_file(bin, &dir, "side-only", "s\n", "side adds a file");
    git(bin, &dir, &["checkout", "-q", "main"]);
    commit_file(bin, &dir, "shared", "one\n2\n3\n4\n5\n6\n7\n8\n9\n", "main edits shared");
    commit_file(bin, &dir, "main-only", "m\n", "main adds a file");
    commit_file(bin, &dir, "shared", "one\n2\n3\n4\nfive\n6\n7\n8\n9\n", "main edits shared again");
    dir
}

/// Run `args` in a fixture of each git and demand the same answer and the same object store.
fn same(label: &str, args: &[&str]) {
    let Some(stock) = stock_git() else { return };
    let mut seen = Vec::new();
    for (bin, side) in [(stock, "stock"), (ZVCS, "zvcs")] {
        let dir = fixture(bin, &format!("{label}-{side}"));
        let run = git(bin, &dir, args);
        let objects = git(bin, &dir, &["cat-file", "--batch-all-objects", "--batch-check"]).stdout;
        let mut lines: Vec<&str> = objects.lines().collect();
        lines.sort_unstable();
        seen.push((run, lines.join("\n"), git(bin, &dir, &["rev-parse", "HEAD"]).stdout));
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }
    assert_eq!(seen[1], seen[0], "git {args:?}: left is zvcs, right is stock");
}

#[test]
fn a_content_merge_dies_before_writing_anything() {
    same("pick", &["--attr-source=nope", "cherry-pick", "side~1"]);
    same("revert", &["--attr-source=nope", "revert", "main~2"]);
    same("merge", &["--attr-source=nope", "merge", "-q", "--no-edit", "side"]);
    same("mtree", &["--attr-source=nope", "merge-tree", "--write-tree", "main", "side"]);
    same("replay", &["--attr-source=nope", "replay", "--onto", "main", "side~2..side"]);
}

#[test]
fn a_range_dies_at_the_first_content_merge() {
    same("range", &["--attr-source=nope", "cherry-pick", "side~2..side"]);
}

#[test]
fn a_resolvable_attr_source_changes_nothing() {
    same("good-pick", &["--attr-source=main", "cherry-pick", "side~1"]);
    same("good-merge", &["--attr-source=main", "merge", "-q", "--no-edit", "side"]);
}

#[test]
fn the_environment_spelling_dies_alike() {
    let Some(stock) = stock_git() else { return };
    let mut seen = Vec::new();
    for (bin, side) in [(stock, "stock"), (ZVCS, "zvcs")] {
        let dir = fixture(bin, &format!("env-{side}"));
        let out = Command::new(bin)
            .args(["cherry-pick", "side~1"])
            .current_dir(&dir)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", dir.parent().unwrap())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_ATTR_SOURCE", "nope")
            .output()
            .unwrap();
        seen.push((out.status.code(), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned()));
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }
    assert_eq!(seen[1], seen[0], "left is zvcs, right is stock");
}
