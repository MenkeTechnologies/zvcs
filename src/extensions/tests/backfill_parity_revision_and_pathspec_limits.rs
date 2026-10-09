//! `git backfill` walks the history the way `walk_objects_by_path()` does, over the revisions,
//! commit-limiting options and pathspecs `setup_revisions()` left it. Only what that walk reaches
//! is fetched from the promisor remote:
//!
//! * the commits `--since`, `--max-count`, `--skip`, a range or `--first-parent` leave, and with a
//!   pathspec only those that touch it (history simplification);
//! * for each commit's root tree, every blob not seen under an earlier path; with pathspecs that
//!   have no wildcard or magic (`exact_pathspecs`) only entries on the way to a pathspec, and a
//!   path's blobs only when the path matches the pathspecs (`walk_path()`);
//! * not the trees of the commits a range excludes, which `mark_edges_uninteresting()` would hand
//!   to `show_edge()` only under `--objects-edge`.
//!
//! zvcs walked everything reachable from `HEAD` whatever the arguments were, so each of these
//! fetched blobs git leaves missing.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn git(bin: &str, dir: &Path, args: &[&str]) -> (String, String, i32) {
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
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

/// A blobless partial clone of a four-commit history over `hist.txt`, `dir/a.txt` and `dir/b.txt`,
/// with committer dates a day apart starting 2023-11-14.
fn partial_clone(label: &str, stock: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-backfill-limits-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let run = |dir: &Path, args: &[&str]| {
        let out = git(stock, dir, args);
        assert_eq!(out.2, 0, "{args:?}: {out:?}");
    };
    run(&root, &["init", "-q", "-b", "main", "src"]);
    let src = root.join("src");
    std::fs::create_dir_all(src.join("dir")).unwrap();
    for (n, (hist, a, b)) in [("h0", "a0", "b0"), ("h1", "a0", "b0"), ("h2", "a2", "b0"), ("h3", "a2", "b3")]
        .into_iter()
        .enumerate()
    {
        std::fs::write(src.join("hist.txt"), format!("{hist}\n")).unwrap();
        std::fs::write(src.join("dir/a.txt"), format!("{a}\n")).unwrap();
        std::fs::write(src.join("dir/b.txt"), format!("{b}\n")).unwrap();
        run(&src, &["add", "."]);
        let date = format!("{} +0000", 1_700_000_000 + 86_400 * n as i64);
        let out = Command::new(stock)
            .args(["commit", "-q", "-m", &format!("c{n}")])
            .current_dir(&src)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .output()
            .unwrap();
        assert!(out.status.success());
    }
    run(&root, &["clone", "-q", "--bare", "src", "remote.git"]);
    run(&root.join("remote.git"), &["config", "uploadpack.allowFilter", "true"]);
    let url = format!("file://{}", root.join("remote.git").display());
    run(&root, &["clone", "-q", "--no-checkout", "--filter=blob:none", &url, "par"]);
    let par = root.join("par");
    (root, par)
}

/// The blobs `backfill <args>` left missing, which is the complement of what it fetched.
fn missing_after(bin: &str, stock: &str, label: &str, args: &[&str]) -> (Vec<String>, i32, String) {
    let (root, par) = partial_clone(label, stock);
    let mut full = vec!["backfill"];
    full.extend_from_slice(args);
    let (stdout, stderr, code) = git(bin, &par, &full);
    let listing = git(stock, &par, &["rev-list", "--objects", "--missing=print", "--all"]).0;
    let mut missing: Vec<String> = listing.lines().filter(|l| l.starts_with('?')).map(str::to_owned).collect();
    missing.sort();
    let _ = std::fs::remove_dir_all(&root);
    (missing, code, format!("{stdout}{stderr}"))
}

fn same(label: &str, args: &[&str]) {
    let Some(stock) = stock_git::stock_git() else { return };
    let want = missing_after(stock, stock, &format!("{label}-stock"), args);
    let got = missing_after(ZVCS, stock, &format!("{label}-zvcs"), args);
    assert_eq!(got, want, "git backfill {args:?}: left is zvcs, right is stock");
}

#[test]
fn commit_limits_choose_the_trees_that_are_walked() {
    same("since-and-range", &["--since=2023-11-15 00:00:00 +0000", "HEAD~3..HEAD"]);
    same("plain", &[]);
    same("max-count", &["--max-count=1"]);
    same("max-count-2", &["-2"]);
    same("skip", &["--skip=2"]);
    same("since", &["--since=2023-11-15 00:00:00 +0000"]);
    same("since-late", &["--since=2024-01-01"]);
    same("until", &["--until=2023-11-14 12:00:00 +0000"]);
    same("first-parent", &["--first-parent", "HEAD~1"]);
}

#[test]
fn ranges_add_the_trees_of_the_commits_they_border_unless_told_not_to() {
    same("range", &["HEAD~2..HEAD"]);
    same("no-edges", &["--no-include-edges", "HEAD~2..HEAD"]);
    same("edges", &["--include-edges", "HEAD~2..HEAD"]);
    same("not", &["HEAD", "--not", "HEAD~2"]);
    same("two-negatives", &["HEAD", "^HEAD~1", "^HEAD~2"]);
    same("range-and-count", &["--max-count=1", "HEAD~3..HEAD"]);
    same("range-and-path", &["HEAD~2..HEAD", "--", "dir/b.txt"]);
    same("caret", &["^HEAD~1", "HEAD"]);
    same("rev", &["HEAD~1"]);
}

#[test]
fn exact_pathspecs_keep_only_the_entries_on_their_way() {
    same("file", &["--", "hist.txt"]);
    same("dir", &["--", "dir"]);
    same("nested", &["--", "dir/a.txt"]);
    same("two", &["--", "hist.txt", "dir/b.txt"]);
    same("absent", &["--", "nosuch"]);
    same("slash", &["--", "dir/"]);
    same("positional", &["HEAD", "dir/a.txt"]);
}

#[test]
fn pathspec_history_simplification_drops_commits_that_do_not_touch_the_path() {
    same("simplified", &["HEAD~3", "--", "dir/b.txt"]);
    same("limited-path", &["--max-count=1", "--", "dir"]);
}

#[test]
fn wildcards_and_magic_are_matched_per_path() {
    same("glob", &["--", "dir/*.txt"]);
    same("magic", &["--", ":(top)dir"]);
    same("exclude", &["--", "dir", ":(exclude)dir/a.txt"]);
}

#[test]
fn the_original_failure() {
    same("original", &["--since=2020-01-01", "--", "HEAD~1", "v0.2.0", "main", "--not", "--since=2020-01-01"]);
}
