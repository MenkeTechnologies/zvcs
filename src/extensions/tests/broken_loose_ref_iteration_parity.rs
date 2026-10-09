//! A loose ref whose file does not parse (a patch, a bundle, a stray note written
//! into `.git/refs/heads/`) in the middle of a ref walk.
//!
//! git yields such a ref with a null id and `REF_ISBROKEN`, and each caller
//! decides: `branch` warns `ignoring broken ref` and lists the rest, `log` and
//! `rev-list` die `bad object <name>` when a pseudo-option selects it,
//! `rev-parse` prints the null id. zvcs failed every one of them with the
//! gitoxide `The reference at ... could not be instantiated`.
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

/// One commit on `main`, `feature`, then a broken file under `refs/heads/` and one
/// under `refs/tags/` and `refs/remotes/o/`.
fn fixture(stock: &str, tag: &str, who: &str) -> Repo {
    let dir = std::env::temp_dir().join(format!("zvcs-brokenref-{tag}-{who}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    for args in [
        &["init", "-q", "-b", "main", "."][..],
        &["commit", "-q", "--allow-empty", "-m", "one"],
        &["branch", "feature"],
    ] {
        let (code, out, err) = run(stock, &dir, args);
        assert_eq!(code, 0, "setup {args:?}: {out}{err}");
    }
    std::fs::create_dir_all(dir.join(".git/refs/remotes/o")).unwrap();
    std::fs::write(dir.join(".git/refs/heads/gen.patch"), "From abc Mon\nSubject: x\n\ndiff\n").unwrap();
    std::fs::write(dir.join(".git/refs/tags/junk"), "not an object id\n").unwrap();
    std::fs::write(dir.join(".git/refs/remotes/o/junk"), "# v2 git bundle\n").unwrap();
    Repo { dir }
}

/// Run `args` in a fresh fixture for stock and for zvcs; the outcomes must be identical.
fn same(tag: &str, args: &[&str]) {
    let Some(stock) = stock_git() else { return };
    let a = fixture(stock, tag, "stock");
    let b = fixture(stock, tag, "zvcs");
    let want = run(stock, &a.dir, args);
    let got = run(ZVCS, &b.dir, args);
    assert_eq!(got, want, "git {args:?}");
}

#[test]
fn branch_warns_and_lists_the_rest() {
    same("branch", &["branch"]);
}

#[test]
fn branch_remotes_only_warns_about_remotes() {
    same("branch-r", &["branch", "-r"]);
}

#[test]
fn branch_all_warns_about_every_namespace() {
    same("branch-a", &["branch", "--list", "-a", "zzz*"]);
}

#[test]
fn log_all_dies_with_the_full_name() {
    same("log-all", &["log", "--oneline", "--all"]);
}

#[test]
fn log_branches_dies_with_the_trimmed_name() {
    same("log-branches", &["log", "--oneline", "--branches"]);
}

#[test]
fn rev_list_tags_dies_on_the_broken_tag() {
    same("rev-list-tags", &["rev-list", "--tags"]);
}

#[test]
fn rev_list_glob_dies_with_the_full_name() {
    same("rev-list-glob", &["rev-list", "--glob=refs/heads/*"]);
}

#[test]
fn rev_list_pattern_that_excludes_it_succeeds() {
    same("rev-list-feat", &["rev-list", "--branches=feat*"]);
}

#[test]
fn rev_parse_all_prints_the_null_id() {
    same("rev-parse-all", &["rev-parse", "--all"]);
}

#[test]
fn rev_parse_branches_prints_the_null_id() {
    same("rev-parse-branches", &["rev-parse", "--branches"]);
}

#[test]
fn log_decorate_ignores_the_broken_ref() {
    same("log-decorate", &["log", "--oneline", "--decorate", "-1"]);
}
