//! `cmd_fetch()` reads `.gitmodules` through `fetch_config_from_gitmodules()` whenever
//! `config.recurse_submodules` is not off (builtin/fetch.c:2632-2637): after
//! `--negotiate-only` has switched recursion off for itself, before `--porcelain` does, and
//! before FETCH_HEAD is truncated or any remote contacted. A `.gitmodules` the config parser
//! refuses therefore ends the command with `fatal: bad config line <n> in file <worktree>/.gitmodules`
//! at 128, whatever the operand names.
//!
//! zvcs only parsed `.gitmodules` to look for `fetch.recurseSubmodules`, swallowed the error,
//! and went on to the remote — leaving an empty FETCH_HEAD behind.

use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir.parent().unwrap())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    let root = dir.parent().unwrap().to_string_lossy().into_owned();
    (
        out.status.code().expect("no signal"),
        String::from_utf8_lossy(&out.stdout).replace(&root, "<root>"),
        String::from_utf8_lossy(&out.stderr).replace(&root, "<root>"),
    )
}

fn fixture(bin: &str, label: &str, gitmodules: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-fetch-gm-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join("repo");
    std::fs::create_dir_all(&dir).unwrap();
    let dir = std::fs::canonicalize(&dir).unwrap();
    run(bin, &dir, &["init", "-q", "-b", "main"]);
    run(bin, &dir, &["commit", "-q", "--allow-empty", "-m", "c"]);
    std::fs::write(dir.join(".gitmodules"), gitmodules).unwrap();
    dir
}

fn same(label: &str, gitmodules: &str, args: &[&str]) {
    let Some(stock) = stock_git() else { return };
    let mut seen = Vec::new();
    for (bin, side) in [(stock, "stock"), (ZVCS, "zvcs")] {
        let dir = fixture(bin, &format!("{label}-{side}"), gitmodules);
        let result = run(bin, &dir, args);
        let fetch_head = dir.join(".git/FETCH_HEAD").exists();
        seen.push((result, fetch_head));
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }
    assert_eq!(seen[1], seen[0], "git {args:?}: left is zvcs, right is stock");
}

#[test]
fn a_refused_gitmodules_ends_fetch_before_the_remote_is_contacted() {
    same("bare", "= 1\n", &["fetch", "nosuch"]);
    same("refspec", "= 1\n", &["fetch", "-k", "+refs/heads/*:refs/remotes/origin/*"]);
    same("dry", "= 1\n", &["fetch", "--dry-run", "nosuch"]);
    same("notkey", "this is not a config line\n", &["fetch", "nosuch"]);
}

#[test]
fn it_is_read_whenever_recursion_is_not_off() {
    same("cli-on", "= 1\n", &["fetch", "--recurse-submodules", "nosuch"]);
    same("cfg-on", "= 1\n", &["-c", "fetch.recurseSubmodules=true", "fetch", "nosuch"]);
    same("porcelain", "= 1\n", &["fetch", "--porcelain", "nosuch"]);
}

#[test]
fn it_is_not_read_when_recursion_is_off() {
    same("cli-off", "= 1\n", &["fetch", "--no-recurse-submodules", "nosuch"]);
    same("cfg-off", "= 1\n", &["-c", "fetch.recurseSubmodules=false", "fetch", "nosuch"]);
    same("sub-off", "= 1\n", &["-c", "submodule.recurse=false", "fetch", "nosuch"]);
}

#[test]
fn a_well_formed_gitmodules_changes_nothing() {
    same("good", "[submodule \"s\"]\n\tpath = s\n\turl = ./s\n", &["fetch", "nosuch"]);
}
