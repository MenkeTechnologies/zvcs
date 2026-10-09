//! `am` is `RUN_SETUP | NEED_WORK_TREE`: git stands in the work tree root before it applies
//! anything, wherever the repository's git directory is — a submodule's lives in the
//! superproject's `.git/modules/<name>`, a `--separate-git-dir` one anywhere. Run from that
//! git directory, `git am` therefore patches the work tree and records its state beside the
//! git directory.
//!
//! zvcs started its `apply` child from the work tree root only when the state directory sat
//! inside the work tree, so from a git directory outside it the patch was written next to the
//! git directory's files and the work tree was left as it was.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn git(bin: &str, dir: &Path, args: &[&str], stdin: &[u8]) -> (i32, String, String) {
    let mut child = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir.ancestors().nth(2).unwrap_or(dir))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
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
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().expect("no signal"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// `root/tree` checked out of the separate git directory `root/gitdir`, on one commit.
fn fixture(stock: &str, label: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-am-outside-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let (tree, gitdir) = (root.join("tree"), root.join("gitdir"));
    let run = |dir: &Path, args: &[&str]| {
        let out = git(stock, dir, args, b"");
        assert_eq!(out.0, 0, "{args:?}: {out:?}");
    };
    run(&root, &["init", "-q", "-b", "main", "--separate-git-dir", gitdir.to_str().unwrap(), "tree"]);
    // What `git submodule` records for a module git directory, so that it can be entered from there.
    run(&tree, &["config", "core.worktree", tree.to_str().unwrap()]);
    std::fs::write(tree.join("old.txt"), "old\n").unwrap();
    run(&tree, &["add", "old.txt"]);
    run(&tree, &["commit", "-q", "-m", "base"]);
    (root, tree, gitdir)
}

const MAIL: &[u8] = b"From: A U Thor <author@example.com>\nSubject: [PATCH] add new\n\n---\ndiff --git a/new.txt b/new.txt\nnew file mode 100644\n--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1 @@\n+hello world\n";
const MAIL_NO_FROM: &[u8] = b"Subject: add new\n\n---\ndiff --git a/new.txt b/new.txt\nnew file mode 100644\n--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1 @@\n+hello world\n";

fn same(label: &str, mail: &[u8], args: &[&str], from_gitdir: bool) {
    let Some(stock) = stock_git() else { return };
    let mut seen = Vec::new();
    for (bin, side) in [(stock, "stock"), (ZVCS, "zvcs")] {
        let (root, tree, gitdir) = fixture(stock, &format!("{label}-{side}"));
        let cwd = if from_gitdir { &gitdir } else { &tree };
        let run = git(bin, cwd, args, mail);
        let shown = root.to_string_lossy().into_owned();
        let files = {
            let mut names: Vec<String> = std::fs::read_dir(&tree)
                .unwrap()
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect();
            names.sort();
            names
        };
        let beside_gitdir = gitdir.join("new.txt").exists();
        let log = git(stock, &tree, &["log", "--oneline"], b"").1;
        seen.push((run.0, run.1.replace(&shown, "<root>"), run.2.replace(&shown, "<root>"), files, beside_gitdir, log.lines().count()));
        let _ = std::fs::remove_dir_all(&root);
    }
    assert_eq!(seen[1], seen[0], "{label}: left is zvcs, right is stock");
}

#[test]
fn am_run_from_the_git_directory_patches_the_work_tree() {
    same("plain", MAIL, &["am"], true);
    same("utf8", MAIL, &["am", "-u"], true);
    same("three-way", MAIL, &["am", "-3"], true);
}

#[test]
fn it_dies_after_applying_when_the_ident_is_empty() {
    same("no-from", MAIL_NO_FROM, &["am"], true);
}

#[test]
fn the_work_tree_root_behaves_as_before() {
    same("from-tree", MAIL, &["am"], false);
}
