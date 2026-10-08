//! `worktree list` prints the checked-out ref through `refs_shorten_unambiguous_ref(…,
//! wt->head_ref, 0)` (builtin/worktree.c, `show_worktree()`): a `HEAD` that is a symbolic ref
//! to something outside `refs/heads/` is shortened by the same rules as any other ref, and a
//! name another ref also answers to keeps its prefix. This port stripped `refs/heads/` only.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "a")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "a")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .output()
        .expect("run git");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

fn fixture(bin: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "zvcs-wt-list-shorten-{}-{}",
        std::process::id(),
        if bin == BIN { "zvcs" } else { "stock" }
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    run(bin, &dir, &["init", "-q", "-b", "main", "."]);
    std::fs::write(dir.join("a"), "a\n").unwrap();
    run(bin, &dir, &["add", "a"]);
    run(bin, &dir, &["commit", "-q", "-m", "one"]);
    run(bin, &dir, &["tag", "foo"]);
    dir
}

/// The list with the repository path (the first column) replaced, since the two fixtures
/// live in different directories.
fn list(bin: &str, dir: &Path, args: &[&str]) -> (String, String, i32) {
    let (out, err, code) = run(bin, dir, args);
    let root = dir.canonicalize().unwrap();
    (out.replace(root.to_str().unwrap(), "<R>"), err, code)
}

#[test]
fn head_ref_is_shortened_like_any_other_ref() {
    let Some(stock) = stock_git() else { return };
    let (s, z) = (fixture(stock), fixture(BIN));
    for target in ["refs/gen-sym", "refs/remotes/o/x", "refs/heads/foo", "refs/heads/plain", "refs/tags/foo"] {
        for d in [&s, &z] {
            run(stock, d, &["symbolic-ref", "HEAD", target]);
        }
        for args in [&["worktree", "list"][..], &["worktree", "list", "--porcelain"], &["worktree", "list", "-v"]] {
            assert_eq!(list(BIN, &z, args), list(stock, &s, args), "HEAD -> {target}: {args:?}");
        }
    }
    let _ = (std::fs::remove_dir_all(s), std::fs::remove_dir_all(z));
}
