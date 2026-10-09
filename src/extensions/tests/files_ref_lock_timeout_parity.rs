//! `core.filesRefLockTimeout` is read by `get_files_ref_lock_timeout_ms()` the first time the files
//! backend locks a loose ref (`lock_raw_ref()`), through `git_config_int()`: a value it cannot parse
//! kills every command that writes a ref with `bad numeric config value … in file <path>`, and
//! leaves the commands that only read refs, or fail before locking, alone. zvcs read the key
//! leniently and wrote the ref.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;
use stock_git::stock_git;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

struct Repo {
    dir: PathBuf,
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn run(bin: &str, dir: &Path, global: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", global)
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.co")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.co")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned().replace(dir.to_str().unwrap(), "<DIR>"),
        String::from_utf8_lossy(&out.stderr).into_owned().replace(dir.to_str().unwrap(), "<DIR>"),
    )
}

/// A repository with one commit, built with a clean configuration, and a global config holding
/// `timeout` as `core.filesRefLockTimeout`.
fn fixture(stock: &str, who: &str, tag: &str, timeout: &str) -> (Repo, PathBuf) {
    let dir = std::env::temp_dir().join(format!("zvcs-reflock-{tag}-{who}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    for args in [&["init", "-q", "-b", "main", "."][..], &["commit", "-q", "--allow-empty", "-m", "one"]] {
        let (code, out, err) = run(stock, &dir, Path::new("/dev/null"), args);
        assert_eq!(code, 0, "setup {args:?}: {out}{err}");
    }
    let global = dir.join("global.config");
    std::fs::write(&global, format!("[core]\n\tfilesRefLockTimeout = {timeout}\n")).unwrap();
    (Repo { dir }, global)
}

/// The outcome of `args` and of listing the refs afterwards, for stock and for zvcs.
fn same(tag: &str, timeout: &str, args: &[&str]) {
    let Some(stock) = stock_git() else { return };
    let (a, ga) = fixture(stock, "stock", tag, timeout);
    let (b, gb) = fixture(stock, "zvcs", tag, timeout);
    let want = run(stock, &a.dir, &ga, args);
    let got = run(ZVCS, &b.dir, &gb, args);
    assert_eq!(got, want, "git {args:?}");
    let refs = ["for-each-ref", "--format=%(refname)"];
    assert_eq!(run(ZVCS, &b.dir, Path::new("/dev/null"), &refs).1, run(stock, &a.dir, Path::new("/dev/null"), &refs).1);
}

#[test]
fn creating_a_tag_dies_on_a_word() {
    same("tag", "always", &["tag", "t1", "HEAD"]);
}

#[test]
fn creating_a_branch_dies_on_an_empty_value() {
    same("branch-empty", "", &["branch", "b1"]);
}

#[test]
fn update_ref_dies_on_a_bad_unit() {
    same("update-ref", "5q", &["update-ref", "refs/heads/u1", "HEAD"]);
}

#[test]
fn deleting_a_ref_dies() {
    same("delete", "always", &["update-ref", "-d", "refs/heads/main"]);
}

#[test]
fn a_commit_dies_when_it_moves_the_branch() {
    same("commit", "always", &["commit", "-q", "--allow-empty", "-m", "two"]);
}

#[test]
fn deleting_a_missing_tag_never_locks() {
    same("missing", "always", &["tag", "-d", "nope"]);
}

#[test]
fn listing_never_locks() {
    same("list", "always", &["branch", "--list"]);
}

#[test]
fn a_negative_timeout_is_valid() {
    same("negative", "-1", &["tag", "t1", "HEAD"]);
}

#[test]
fn a_unit_suffix_is_valid() {
    same("unit", "1k", &["tag", "t1", "HEAD"]);
}
