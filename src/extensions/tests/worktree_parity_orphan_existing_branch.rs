//! `git worktree add --orphan -b <branch>` when `<branch>` already exists.
//!
//! add_worktree() treats an existing branch as `is_branch` even under `--orphan`,
//! so one checked out elsewhere dies `'<branch>' is already used by worktree at`
//! unless `-f` (builtin/worktree.c:482-487). Otherwise make_worktree_orphan() calls
//! validate_new_branchname(ref, &symref, 0) in the parent, which dies `a branch
//! named '<branch>' already exists` whatever `-f` says (worktree.c:409-416,
//! branch.c:505-514), and the atexit remove_junk() deletes the half-made
//! administrative directory and work tree (worktree.c:273-288). zvcs created the
//! worktree on the existing branch and exited 0.
//!
//! Skipped when no stock git is available.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str]) -> Output {
    let mut c = Command::new(bin);
    for (k, _) in std::env::vars() {
        if k.starts_with("GIT_") {
            c.env_remove(k);
        }
    }
    c.args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env_remove("XDG_CONFIG_HOME")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .output()
        .expect("spawn git")
}

/// `repo` on `main` with a second branch `side`, at the same path for each side.
fn fixture(stock: &str, root: &Path) -> PathBuf {
    let _ = std::fs::remove_dir_all(root);
    std::fs::create_dir_all(root).unwrap();
    let root = root.canonicalize().unwrap();
    assert!(run(stock, &root, &["init", "-q", "-b", "main", "repo"]).status.success());
    let repo = root.join("repo");
    assert!(run(stock, &repo, &["commit", "-q", "--allow-empty", "-m", "one"]).status.success());
    assert!(run(stock, &repo, &["branch", "side"]).status.success());
    repo
}

/// stdout, stderr and exit code of `args`, then the worktree list and what is left
/// of `n1` and the administrative directory.
fn observe(bin: &str, stock: &str, repo: &Path, args: &[&str]) -> String {
    let out = run(bin, repo, args);
    let list = run(stock, repo, &["worktree", "list", "--porcelain"]);
    format!(
        "{}\n--\n{}\n-- {:?}\n{}\nn1: {}\nadmin: {}\n",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
        out.status.code(),
        String::from_utf8_lossy(&list.stdout),
        repo.parent().unwrap().join("n1").exists(),
        repo.join(".git/worktrees").exists(),
    )
}

#[test]
fn an_existing_branch_refuses_orphan_and_leaves_nothing_behind() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = std::env::temp_dir().join(format!("zvcs-wt-orphan-exists-{}", std::process::id()));
    let cases: &[&[&str]] = &[
        &["worktree", "add", "--orphan", "-b", "side", "../n1"],
        &["worktree", "add", "-f", "--orphan", "-b", "side", "../n1"],
        &["worktree", "add", "--orphan", "-b", "main", "../n1"],
        &["worktree", "add", "-f", "--orphan", "-b", "main", "../n1"],
        &["worktree", "add", "--orphan", "-b", "fresh", "../n1"],
    ];
    for args in cases {
        let repo = fixture(stock, &root);
        let want = observe(stock, stock, &repo, args);
        let repo = fixture(stock, &root);
        let got = observe(BIN, stock, &repo, args);
        assert_eq!(got, want, "{args:?}");
    }
    let _ = std::fs::remove_dir_all(&root);
}
