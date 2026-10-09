//! `cmd_reflog()` gives `list`, `exists`, `delete`, `drop` and `write` subcommands that never run
//! `git_default_config`, so a configuration value that callback refuses (here
//! `color.advice.reset`) kills `reflog show` and `reflog expire` but not those five. `list`,
//! `exists` and `drop` also read no object, so the repository-settings block is not prepared for
//! them either. zvcs ran the default walk for every `reflog` subcommand.
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

fn fixture(stock: &str, tag: &str, who: &str, global_config: &str) -> (Repo, PathBuf) {
    let dir = std::env::temp_dir().join(format!("zvcs-reflogcfg-{tag}-{who}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    let global = dir.join("global.config");
    std::fs::write(&global, global_config).unwrap();
    for args in [&["init", "-q", "-b", "main", "."][..], &["commit", "-q", "--allow-empty", "-m", "one"]] {
        let (code, out, err) = run(stock, &dir, Path::new("/dev/null"), args);
        assert_eq!(code, 0, "setup {args:?}: {out}{err}");
    }
    (Repo { dir }, global)
}

fn same(tag: &str, global_config: &str, args: &[&str]) {
    let Some(stock) = stock_git() else { return };
    let (a, ga) = fixture(stock, tag, "stock", global_config);
    let (b, gb) = fixture(stock, tag, "zvcs", global_config);
    let want = run(stock, &a.dir, &ga, args);
    let got = run(ZVCS, &b.dir, &gb, args);
    assert_eq!(got, want, "git {args:?}");
}

const BAD_COLOR: &str = "[color \"advice\"]\n\treset = off\n";

#[test]
fn delete_without_an_operand_reports_its_own_error() {
    same("delete", BAD_COLOR, &["reflog", "delete", "--no-updateref", "--dry-run"]);
}

#[test]
fn list_lists() {
    same("list", BAD_COLOR, &["reflog", "list"]);
}

#[test]
fn exists_answers() {
    same("exists", BAD_COLOR, &["reflog", "exists", "refs/heads/main"]);
}

#[test]
fn drop_all_succeeds() {
    same("drop", BAD_COLOR, &["reflog", "drop", "--all"]);
}

#[test]
fn write_reports_its_usage() {
    same("write", BAD_COLOR, &["reflog", "write", "refs/heads/zz"]);
}

#[test]
fn expire_still_refuses_the_value() {
    same("expire", BAD_COLOR, &["reflog", "expire", "--all", "--dry-run"]);
}

#[test]
fn show_still_refuses_the_value() {
    same("show", BAD_COLOR, &["reflog", "show"]);
}

#[test]
fn list_skips_the_settings_block() {
    same("list-settings", "[core]\n\tpackedGitLimit = bogus\n", &["reflog", "list"]);
}

#[test]
fn drop_skips_the_settings_block() {
    same("drop-settings", "[core]\n\tpackedGitLimit = bogus\n", &["reflog", "drop", "--all"]);
}

#[test]
fn delete_still_prepares_the_settings_block() {
    same("delete-settings", "[core]\n\tdeltaBaseCacheLimit = -1\n", &["reflog", "delete", "--dry-run", "HEAD@{0}"]);
}
