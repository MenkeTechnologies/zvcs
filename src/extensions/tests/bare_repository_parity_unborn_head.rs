//! Verbs against stock git inside a bare repository whose HEAD names no commit yet.
//!
//! Attributes come from the work tree, and a bare repository has none: git's default
//! attribute source is `HEAD`, and an unborn one is ignored, so `diff --cached` compares an
//! empty index with the empty tree and succeeds. `setup_bare_git_dir()` also leaves git in
//! the git directory itself, so `rev-parse HEAD` finds the file of that name
//! (`check_filename()`) and echoes it instead of dying `ambiguous argument`.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

type Outcome = (String, String, Option<i32>);

fn run(bin: &str, dir: &Path, args: &[&str]) -> Outcome {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (text(&out.stdout), text(&out.stderr), out.status.code())
}

#[test]
fn an_unborn_head_in_a_bare_repository() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = std::env::temp_dir().join(format!("zvcs-bare-unborn-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    run(stock, &root, &["init", "-q", "--bare", "-b", "main"]);

    let cases: &[&[&str]] = &[
        &["diff", "--cached"],
        &["diff", "--cached", "--name-status"],
        &["diff", "--cached", "--stat"],
        &["diff", "--cached", "-p"],
        &["rev-parse", "HEAD"],
        &["rev-parse", "--symbolic", "HEAD", "config"],
        &["rev-parse", "nope"],
        &["rev-parse", "--verify", "HEAD"],
    ];
    for args in cases {
        assert_eq!(run(BIN, &root, args), run(stock, &root, args), "{args:?}");
    }
    let _ = std::fs::remove_dir_all(&root);
}
