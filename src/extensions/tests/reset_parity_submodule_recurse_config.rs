//! `submodule.recurse` that is not a boolean, under `reset`, `read-tree` and `merge --abort`.
//!
//! `git_reset_config()` hands `submodule.recurse` to `git_default_submodule_config()`, which
//! reads it with `git_config_bool()` and dies on a value that is not one — before `reset`
//! parses a single option or looks at the repository's state. `read-tree` reads it the same
//! way, and `merge --abort` is a `reset --merge` child. zvcs read the key leniently and went
//! on to the verb's own business.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, envs: &[(&str, &str)], args: &[&str]) -> (String, String, Option<i32>) {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "a")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "a")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("HOME", dir)
        .env("LC_ALL", "C");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code(),
    )
}

/// `main` and `theirs` conflict, so a merge is in progress.
fn fixture(tag: &str, stock: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-subrecurse-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    run(stock, &dir, &[], &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("f.txt"), "base\n").unwrap();
    run(stock, &dir, &[], &["add", "."]);
    run(stock, &dir, &[], &["commit", "-qm", "base"]);
    run(stock, &dir, &[], &["checkout", "-q", "-b", "theirs"]);
    std::fs::write(dir.join("f.txt"), "theirs\n").unwrap();
    run(stock, &dir, &[], &["commit", "-qam", "theirs"]);
    run(stock, &dir, &[], &["checkout", "-q", "main"]);
    std::fs::write(dir.join("f.txt"), "ours\n").unwrap();
    run(stock, &dir, &[], &["commit", "-qam", "ours"]);
    assert_eq!(run(stock, &dir, &[], &["merge", "theirs"]).2, Some(1));
    dir
}

#[test]
fn a_non_boolean_submodule_recurse_stops_these_verbs_first() {
    let Some(stock) = stock_git() else { return };
    let cases: [&[&str]; 9] = [
        &["reset", "--merge"],
        &["reset", "--hard"],
        &["reset"],
        &["reset", "--soft", "HEAD"],
        &["reset", "-p"],
        &["reset", "-q", "HEAD", "--", "f.txt"],
        &["merge", "--abort"],
        &["read-tree", "HEAD"],
        &["reset", "-h"],
    ];
    for value in ["\t", "abc", "no"] {
        let env = [
            ("GIT_CONFIG_COUNT", "1"),
            ("GIT_CONFIG_KEY_0", "submodule.recurse"),
            ("GIT_CONFIG_VALUE_0", value),
        ];
        for args in cases {
            let mut seen = Vec::new();
            for (who, bin) in [("stock", stock), ("zvcs", BIN)] {
                let dir = fixture(who, stock);
                let result = run(bin, &dir, &env, args);
                let state = run(stock, &dir, &[], &["status", "--porcelain"]).0;
                let _ = std::fs::remove_dir_all(&dir);
                seen.push((result, state));
            }
            assert_eq!(seen[1], seen[0], "value {value:?} args {args:?}");
        }
    }
}
