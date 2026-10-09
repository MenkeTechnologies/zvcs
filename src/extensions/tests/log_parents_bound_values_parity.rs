//! `--min-parents=<n>` and `--max-parents=<n>` take `parse_count()` (`strtol_i`): leading
//! whitespace and a sign are accepted, so `--max-parents=" 1"` and `--min-parents=-1` are valid,
//! a negative `--max-parents` is "no limit" and a negative `--min-parents` constrains nothing.
//! zvcs accepted bare digits only and died `' 1': not an integer`.
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

fn run(bin: &str, dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
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
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// root, a side branch commit, a main commit and the merge of the two.
fn fixture(stock: &str, who: &str, tag: &str) -> Repo {
    let dir = std::env::temp_dir().join(format!("zvcs-parents-{tag}-{who}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    for args in [
        &["init", "-q", "-b", "main", "."][..],
        &["commit", "-q", "--allow-empty", "-m", "root"],
        &["checkout", "-q", "-b", "side"],
        &["commit", "-q", "--allow-empty", "-m", "side"],
        &["checkout", "-q", "main"],
        &["commit", "-q", "--allow-empty", "-m", "main"],
        &["merge", "-q", "--no-ff", "-m", "merge", "side"],
    ] {
        let (code, out, err) = run(stock, &dir, args);
        assert_eq!(code, 0, "setup {args:?}: {out}{err}");
    }
    Repo { dir }
}

fn same(tag: &str, args: &[&str]) {
    let Some(stock) = stock_git() else { return };
    let a = fixture(stock, "stock", tag);
    let b = fixture(stock, "zvcs", tag);
    let want = run(stock, &a.dir, args);
    let got = run(ZVCS, &b.dir, args);
    assert_eq!(got, want, "git {args:?}");
}

#[test]
fn max_parents_accepts_leading_whitespace() {
    same("max-space", &["log", "--oneline", "--max-parents= 1"]);
}

#[test]
fn min_parents_accepts_leading_whitespace() {
    same("min-space", &["log", "--oneline", "--min-parents= 2"]);
}

#[test]
fn negative_max_parents_is_no_limit() {
    same("max-neg", &["log", "--oneline", "--max-parents=-1"]);
}

#[test]
fn negative_min_parents_constrains_nothing() {
    same("min-neg", &["log", "--oneline", "--min-parents=-3"]);
}

#[test]
fn explicit_plus_sign_is_read() {
    same("plus", &["log", "--oneline", "--max-parents=+1"]);
}

#[test]
fn empty_value_is_not_an_integer() {
    same("empty", &["log", "--oneline", "--max-parents="]);
}

#[test]
fn trailing_garbage_is_not_an_integer() {
    same("trail", &["log", "--oneline", "--min-parents=1x"]);
}
