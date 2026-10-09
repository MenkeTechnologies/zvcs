//! `git merge --continue` runs `git commit` as a child.
//!
//! `cmd_merge()` hands the commit to a `commit` child whose `git_config(git_commit_config)`
//! runs before it looks at the index, so a value that callback refuses (`status.showStash`,
//! `diff.context`) is the `fatal:` and 128 — not the unmerged-paths report the commit would
//! reach next. zvcs checked the index first.

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

fn conflicted(tag: &str, stock: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-mergecont-{tag}-{}", std::process::id()));
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
fn the_commit_childs_config_is_read_before_the_index_is_judged() {
    let Some(stock) = stock_git() else { return };
    let envs: [&[(&str, &str)]; 3] = [
        &[],
        &[("GIT_CONFIG_COUNT", "1"), ("GIT_CONFIG_KEY_0", "status.showStash"), ("GIT_CONFIG_VALUE_0", "always")],
        &[("GIT_CONFIG_COUNT", "1"), ("GIT_CONFIG_KEY_0", "diff.context"), ("GIT_CONFIG_VALUE_0", "no")],
    ];
    for env in envs {
        let mut seen = Vec::new();
        for (who, bin) in [("stock", stock), ("zvcs", BIN)] {
            let dir = conflicted(who, stock);
            let result = run(bin, &dir, env, &["merge", "--continue"]);
            let _ = std::fs::remove_dir_all(&dir);
            seen.push(result);
        }
        assert_eq!(seen[1], seen[0], "env {env:?}");
    }
}
