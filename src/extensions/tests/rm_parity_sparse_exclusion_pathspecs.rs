//! `do_match_pathspec()` (dir.c:550-586) tries every item against a path, records the strongest
//! match of each in `seen[]` — also for a path a later exclusion removes — and sets an
//! exclusion's own `seen[]` to "matched" as soon as a positive item matched anything
//! (`seen[i] = MATCHED_FNMATCH`, "Make exclude patterns optional and never report `pathspec
//! ':(exclude)foo' matches no files`"). `git rm` reads that array twice: over the visible
//! entries, and, for items that matched nothing there, over the sparse-checkout's skip-worktree
//! entries (`find_pathspecs_matching_skip_worktree()`), where a hit turns "did not match any files"
//! into the sparse-path report and exit 1. It judges the exclusions like any other item.
//!
//! zvcs asked its matcher for the first matching pattern only, so a path that an exclusion removed
//! left no trace in either array, and `git rm -- src/lib.rs :^src` over a skip-worktree
//! `src/lib.rs` died `did not match any files` where git prints the sparse report.

use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn git(bin: &str, dir: &Path, args: &[&str]) -> (i32, String, String) {
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
        out.status.code().expect("no signal"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A cone checkout of `inside` over a tree that also has `outside/…` and `src/lib.rs`.
fn fixture(stock: &str, label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-rm-sparse-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join("repo");
    std::fs::create_dir_all(&dir).unwrap();
    let dir = std::fs::canonicalize(&dir).unwrap();
    let run = |args: &[&str]| {
        let out = git(stock, &dir, args);
        assert_eq!(out.0, 0, "{args:?}: {out:?}");
    };
    run(&["init", "-q", "-b", "main"]);
    for path in ["README.md", "root.txt", "inside/keep.txt", "inside/nested/also.txt", "outside/drop.txt", "outside/nested/deep.txt", "src/lib.rs"] {
        let full = dir.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, format!("{path}\n")).unwrap();
    }
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "tree"]);
    run(&["sparse-checkout", "set", "--cone", "inside"]);
    dir
}

fn same(label: &str, args: &[&str]) {
    let Some(stock) = stock_git() else { return };
    let mut seen = Vec::new();
    for (bin, side) in [(stock, "stock"), (ZVCS, "zvcs")] {
        let dir = fixture(stock, &format!("{label}-{side}"));
        let run = git(bin, &dir, args);
        let listing = git(stock, &dir, &["ls-files", "-v"]).1;
        let root = dir.parent().unwrap().to_string_lossy().into_owned();
        seen.push((run.0, run.1.replace(&root, "<root>"), run.2.replace(&root, "<root>"), listing));
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }
    assert_eq!(seen[1], seen[0], "git {args:?}: left is zvcs, right is stock");
}

#[test]
fn an_excluded_sparse_path_still_earns_the_sparse_report() {
    same("lib-excl-src", &["rm", "--", "src/lib.rs", ":^src"]);
    same("excl-first", &["rm", "--", ":^src", "src/lib.rs"]);
    same("dry-run", &["rm", "-n", "--", "src/lib.rs", ":^src"]);
    same("magic-spelling", &["rm", "--", "src/lib.rs", ":(exclude)src"]);
}

#[test]
fn with_sparse_the_path_is_removed_whatever_excludes_it_in_the_report() {
    same("sparse-flag", &["rm", "--sparse", "src/lib.rs", ":^src"]);
    same("sparse-recursive", &["rm", "--sparse", "-r", "--", "outside", ":^outside/nested"]);
}

#[test]
fn recursive_specs_over_sparse_and_visible_entries() {
    same("outside-nested", &["rm", "-r", "--", "outside", ":^outside/nested"]);
    same("outside-no-r", &["rm", "--", "outside", ":^outside/nested"]);
    same("mixed", &["rm", "-n", "-r", "--", "outside", "inside", ":^outside/drop.txt"]);
    same("inside", &["rm", "-n", "-r", "--", "inside", ":^inside/nested"]);
}

#[test]
fn exclusions_that_match_nothing_stay_optional() {
    same("optional", &["rm", "-n", "--", "README.md", ":^nomatch"]);
    same("two-excludes", &["rm", "-n", "--", "root.txt", ":^nomatch", ":^README.md"]);
    same("unmatched-positive", &["rm", "--", "nosuch", ":^src"]);
    same("only-exclusions", &["rm", "-n", "-r", ":^src"]);
    same("ignore-unmatch", &["rm", "--ignore-unmatch", "nosuch", ":^x"]);
}
