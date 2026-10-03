//! `git tag`'s listing filters against stock git: each is an option callback
//! (builtin/tag.c `options[]`) that resolves its operand while argv is walked,
//! so a bad operand is reported before later options and post-parse checks,
//! and every occurrence appends to the filter's list rather than replacing it.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str]) -> (String, String, Option<i32>) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("LC_ALL", "C")
        .env("GIT_EDITOR", "true")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@x")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (text(&out.stdout), text(&out.stderr), out.status.code())
}

#[test]
fn filters_resolve_in_argv_order_and_accumulate() {
    let Some(stock) = stock_git::stock_git_at_least((2, 56, 0)) else { return };
    let root = std::env::temp_dir().join(format!("zvcs-tag-filters-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let git = |args: &[&str]| run(stock, &root, args);
    git(&["init", "-q", "-b", "main"]);
    git(&["commit", "-q", "--allow-empty", "-m", "one"]);
    git(&["tag", "v1"]);
    git(&["branch", "topic"]);
    git(&["commit", "-q", "--allow-empty", "-m", "two"]);
    git(&["tag", "t4"]);
    git(&["checkout", "-q", "topic"]);
    git(&["commit", "-q", "--allow-empty", "-m", "three"]);
    git(&["tag", "t3"]);
    git(&["checkout", "-q", "main"]);

    for args in [
        &["tag", "--contains", "v1", "--contains", "topic"][..],
        &["tag", "--no-contains", "topic", "--no-contains", "main"],
        &["tag", "--merged", "topic", "--merged", "main"],
        &["tag", "--no-merged", "topic", "--no-merged", "v1"],
        &["tag", "--contains=nope", "--badopt"],
        &["tag", "--contains", "nope", "--sort=bad"],
        &["tag", "--merged", "nope", "--contains", "bad"],
        &["tag", "--no-contains=false", "-n1", "--column"],
    ] {
        assert_eq!(run(BIN, &root, args), run(stock, &root, args), "{args:?}");
    }
    let _ = std::fs::remove_dir_all(&root);
}
