//! The selectors refresh the index, and `refresh_index()` names every conflicted path.
//!
//! `run_add_p()` calls `repo_refresh_and_write_index(r, REFRESH_QUIET, …)` before the
//! selector for every patch mode that is not `index_only`, and `patch_update_file()`
//! again after it applies a selection; `run_add_i()` does it once before its first menu.
//! `REFRESH_QUIET` silences the stat-dirty report but not the unmerged one, so each
//! conflicted path is printed `<path>: needs merge` on stdout, once per refresh.
//!
//! Expectations come from stock git (`support/stock_git.rs`) in an identical repository.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn git(bin: &str, dir: &Path, stdin: &str, args: &[&str]) -> (String, String, i32) {
    let mut child = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir.parent().unwrap())
        .env("GIT_CEILING_DIRECTORIES", dir.parent().unwrap())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

/// A repository with an unmerged `c.txt` and a modified, tracked `a.txt`.
fn conflicted(label: &str, bin: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-patch-refresh-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let work = std::fs::canonicalize(&work).unwrap();
    let run = |args: &[&str]| {
        git(bin, &work, "", args);
    };
    run(&["init", "-q", "-b", "main", "."]);
    std::fs::write(work.join("a.txt"), "a\n").unwrap();
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "base"]);
    run(&["checkout", "-q", "-b", "side"]);
    std::fs::write(work.join("c.txt"), "side\n").unwrap();
    run(&["add", "c.txt"]);
    run(&["commit", "-q", "-m", "side"]);
    run(&["checkout", "-q", "main"]);
    std::fs::write(work.join("c.txt"), "main\n").unwrap();
    run(&["add", "c.txt"]);
    run(&["commit", "-q", "-m", "main"]);
    run(&["merge", "-q", "side"]);
    std::fs::write(work.join("a.txt"), "a\nmore\n").unwrap();
    work
}

fn same(label: &str, stdin: &str, args: &[&str]) {
    let Some(stock) = stock_git::stock_git() else { return };
    let s = conflicted(&format!("{label}-stock"), stock);
    let z = conflicted(&format!("{label}-zvcs"), ZVCS);
    let want = git(stock, &s, stdin, args);
    let got = git(ZVCS, &z, stdin, args);
    let _ = std::fs::remove_dir_all(s.parent().unwrap());
    let _ = std::fs::remove_dir_all(z.parent().unwrap());
    assert_eq!(got, want, "git {args:?} with stdin {stdin:?}: left is zvcs, right is stock");
    assert!(
        want.0.contains("c.txt: needs merge"),
        "the oracle no longer reports the conflict, the test checks nothing: {want:?}"
    );
}

#[test]
fn add_patch_refreshes_before_the_selector() {
    same("add-p-start", "q\n", &["add", "-p"]);
}

#[test]
fn add_patch_refreshes_again_after_applying() {
    same("add-p-apply", "y\n", &["add", "-p"]);
}

#[test]
fn a_declined_hunk_does_not_refresh_again() {
    same("add-p-decline", "n\n", &["add", "-p"]);
}

#[test]
fn worktree_modes_refresh_before_the_selector() {
    same("checkout-p", "n\n", &["checkout", "-p"]);
    same("restore-p", "y\n", &["restore", "-p"]);
}

#[test]
fn checkout_patch_against_a_tree_refreshes_after_the_worktree_fallback() {
    same("checkout-p-head", "y\n", &["checkout", "-p", "HEAD"]);
}

#[test]
fn index_only_mode_refreshes_only_after_applying() {
    let Some(stock) = stock_git::stock_git() else { return };
    let s = conflicted("reset-p-stock", stock);
    let z = conflicted("reset-p-zvcs", ZVCS);
    for (bin, dir) in [(stock, &s), (ZVCS, &z)] {
        git(bin, dir, "", &["add", "a.txt"]);
    }
    let want = git(stock, &s, "q\n", &["reset", "-p"]);
    let got = git(ZVCS, &z, "q\n", &["reset", "-p"]);
    let _ = std::fs::remove_dir_all(s.parent().unwrap());
    let _ = std::fs::remove_dir_all(z.parent().unwrap());
    assert_eq!(got, want);
    assert!(!want.0.contains("needs merge"), "{want:?}");
}

#[test]
fn add_interactive_refreshes_before_the_first_menu() {
    same("add-i", "q\n", &["add", "-i"]);
}
