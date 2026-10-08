//! `git switch --orphan` / `git checkout --orphan` against stock git.
//!
//! `switch --orphan <new>` clears the tracked worktree and leaves an **empty index**
//! (`orphan_from_empty_tree`, builtin/checkout.c). zvcs emptied the index correctly
//! and then the index refreshed by the clean-gate, still held for the command's final
//! flush, was written back over it: the worktree was empty while `ls-files` listed
//! every old path and `status` reported each as `AD`.

use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "support/stock_git.rs"]
mod stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str]) -> (i32, String) {
    let out = Command::new(bin)
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.code().unwrap_or(-1), text)
}

fn fixture(tag: &str, bin: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-orphan-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("README.md"), "a\n").unwrap();
    std::fs::write(dir.join("src/lib.rs"), "b\n").unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["add", "."],
        &["commit", "-q", "-m", "one"],
    ] {
        assert_eq!(run(bin, &dir, args).0, 0, "{args:?}");
    }
    dir
}

/// Observable post-state: exit code, status, staged paths, worktree listing, HEAD.
fn state(bin: &str, dir: &Path) -> String {
    let mut s = String::new();
    for args in [
        &["status", "--short"][..],
        &["ls-files", "-s"],
        &["symbolic-ref", "HEAD"],
        &["for-each-ref", "--format=%(refname)"],
    ] {
        s.push_str(&format!("# {args:?}\n{}\n", run(bin, dir, args).1));
    }
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n != ".git")
        .collect();
    names.sort();
    s.push_str(&format!("# worktree {names:?}\n"));
    s
}

fn assert_matches_stock(tag: &str, args: &[&str]) {
    let Some(stock) = stock_git::stock_git() else { return };
    let (z, s) = (fixture(&format!("{tag}-z"), BIN), fixture(&format!("{tag}-s"), stock));
    let (zrc, _) = run(BIN, &z, args);
    let (src, _) = run(stock, &s, args);
    assert_eq!(zrc, src, "exit code of {args:?}");
    let (zs, ss) = (state(BIN, &z), state(stock, &s));
    let _ = std::fs::remove_dir_all(&z);
    let _ = std::fs::remove_dir_all(&s);
    assert_eq!(zs, ss, "post-state of {args:?}");
}

#[test]
fn switch_orphan_leaves_empty_index() {
    let dir = fixture("empty", BIN);
    assert_eq!(run(BIN, &dir, &["switch", "-q", "--orphan", "fresh"]).0, 0);
    let (_, staged) = run(BIN, &dir, &["ls-files", "-s"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(staged, "", "index must be empty after switch --orphan");
}

#[test]
fn switch_orphan_matches_stock() {
    assert_matches_stock("switch", &["switch", "-q", "--orphan", "fresh"]);
}

#[test]
fn checkout_orphan_matches_stock() {
    assert_matches_stock("checkout", &["checkout", "-q", "--orphan", "fresh"]);
}
