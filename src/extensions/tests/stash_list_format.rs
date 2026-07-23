//! `git stash list [--format=…]` — git-fuzzy's stash view runs
//! `stash list --format='%H %gd: %gs'`. zvcs implements this by delegating to the
//! reflog machinery on `refs/stash`, so the log placeholders (`%H`, `%gd`, `%gs`,
//! dates, `--oneline`) all work. Each case is diffed against real git byte-for-byte,
//! including the `%gd` shortening (`refs/stash` → `stash`) that differs from the
//! oneline selector (which keeps the ref as typed).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");
const DATE: &str = "1136214245 +0000";

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_DATE", DATE)
        .env("GIT_COMMITTER_DATE", DATE)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?} failed");
}

/// A repo with one commit and one stash entry, plus a `home` for HOME/ZVCS_HOME.
fn fixture() -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-stashfmt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "a@b"]);
    git(&repo, &["config", "user.name", "A"]);
    std::fs::write(repo.join("f"), "one\n").unwrap();
    git(&repo, &["add", "f"]);
    git(&repo, &["-c", "commit.gpgsign=false", "commit", "-q", "-m", "c0", "--date", DATE]);
    std::fs::write(repo.join("f"), "one\ntwo\n").unwrap();
    git(&repo, &["stash"]);
    (repo, home)
}

fn run(bin: &str, repo: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(bin)
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home)
        .env("GIT_TEST_DATE_NOW", "1200000000")
        .output()
        .unwrap()
}

fn assert_matches(repo: &Path, home: &Path, args: &[&str]) {
    let mine = run(BIN, repo, home, args);
    let theirs = run("git", repo, home, args);
    assert_eq!(
        String::from_utf8_lossy(&theirs.stdout),
        String::from_utf8_lossy(&mine.stdout),
        "`{args:?}` must match real git byte-for-byte"
    );
}

#[test]
fn stash_list_formats_match_git() {
    let (repo, home) = fixture();

    // git-fuzzy's exact format, the default, oneline, and the %gd/%gD split.
    assert_matches(&repo, &home, &["stash", "list", "--format=%H %gd: %gs"]);
    assert_matches(&repo, &home, &["stash", "list"]);
    assert_matches(&repo, &home, &["stash", "list", "--oneline"]);
    assert_matches(&repo, &home, &["stash", "list", "--format=%gd|%gD|%ci|%s"]);

    // The reflog selector shortening the delegation relies on: %gd shortens
    // refs/stash to stash, %gD keeps the full ref, oneline keeps it as typed.
    assert_matches(&repo, &home, &["reflog", "show", "refs/stash", "--format=%gd"]);
    assert_matches(&repo, &home, &["reflog", "show", "refs/stash", "--format=%gD"]);
    assert_matches(&repo, &home, &["reflog", "show", "refs/stash"]);

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn stash_list_empty_without_a_stash() {
    let root = std::env::temp_dir().join(format!("zvcs-stashempty-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let (home, repo) = (root.join("home"), root.join("repo"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);

    // No stash ref → empty stdout, exit 0 (not a fatal on the missing ref).
    let out = run(BIN, &repo, &home, &["stash", "list", "--format=%H %gd: %gs"]);
    assert!(out.status.success());
    assert!(out.stdout.is_empty(), "stash list with no stash must be empty");

    let _ = std::fs::remove_dir_all(&root);
}
