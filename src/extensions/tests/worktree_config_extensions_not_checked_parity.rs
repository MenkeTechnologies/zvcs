//! `read_repository_format()` runs `check_repo_format()` over `<commondir>/config` only. An
//! `extensions.<key>` line in `config.worktree` (read when `extensions.worktreeConfig` is on) is
//! ordinary configuration to git and is never examined, so a value `handle_extension()` would
//! refuse there changes nothing. zvcs ran the refusal over `config.worktree` too and died
//! `invalid value for 'extensions.refstorage': 'bogus'`.
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

/// A repository with `extensions.worktreeConfig` on, `worktree_config` as its `config.worktree`
/// and `extra` appended to `.git/config`.
fn fixture(stock: &str, who: &str, tag: &str, worktree_config: &str, extra: &str) -> Repo {
    let dir = std::env::temp_dir().join(format!("zvcs-wtext-{tag}-{who}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    for args in [
        &["init", "-q", "-b", "main", "."][..],
        &["commit", "-q", "--allow-empty", "-m", "one"],
        &["config", "extensions.worktreeConfig", "true"],
    ] {
        let (code, out, err) = run(stock, &dir, args);
        assert_eq!(code, 0, "setup {args:?}: {out}{err}");
    }
    std::fs::write(dir.join(".git/config.worktree"), worktree_config).unwrap();
    if !extra.is_empty() {
        let mut config = std::fs::read_to_string(dir.join(".git/config")).unwrap();
        config.push_str(extra);
        std::fs::write(dir.join(".git/config"), config).unwrap();
    }
    Repo { dir }
}

fn same(tag: &str, worktree_config: &str, extra: &str, args: &[&str]) {
    let Some(stock) = stock_git() else { return };
    let a = fixture(stock, "stock", tag, worktree_config, extra);
    let b = fixture(stock, "zvcs", tag, worktree_config, extra);
    let want = run(stock, &a.dir, args);
    let got = run(ZVCS, &b.dir, args);
    assert_eq!(got, want, "git {args:?}");
}

#[test]
fn a_refused_ref_storage_in_config_worktree_is_ignored() {
    same("refstorage", "[extensions]\n\trefStorage = bogus\n", "", &["status", "--porcelain=v2", "--branch"]);
}

#[test]
fn a_refused_object_format_in_config_worktree_is_ignored() {
    same("objectformat", "[extensions]\n\tobjectFormat = bogus\n", "", &["log", "--oneline"]);
}

#[test]
fn a_valueless_core_worktree_in_config_worktree_is_still_refused() {
    same("valueless", "[core]\n\tworktree\n", "", &["rev-parse", "HEAD"]);
}

#[test]
fn the_same_value_in_the_main_config_is_still_refused() {
    same("main-config", "", "[extensions]\n\trefStorage = bogus\n", &["status", "--porcelain"]);
}
