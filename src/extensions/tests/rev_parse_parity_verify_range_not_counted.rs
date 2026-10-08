//! `git rev-parse --verify`/`--short` against stock git: a range does not count
//! as a revision.
//!
//! `try_difference()` prints a `a..b` range's endpoints and `continue`s before the
//! `revs_count++` of the single-revision path, so `--verify a..b HEAD` still holds
//! exactly one revision and prints it after the range, while `--verify a..b` alone
//! has none and fails `Needed a single revision`.
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
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@x")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (text(&out.stdout), text(&out.stderr), out.status.code())
}

#[test]
fn a_range_is_printed_but_never_counted() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = std::env::temp_dir().join(format!("zvcs-rp-verify-range-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    run(stock, &root, &["init", "-q", "-b", "main"]);
    run(stock, &root, &["commit", "-q", "--allow-empty", "-m", "one"]);
    run(stock, &root, &["commit", "-q", "--allow-empty", "-m", "two"]);

    let cases: &[&[&str]] = &[
        &["rev-parse", "--verify", "HEAD~1..HEAD"],
        &["rev-parse", "--verify", "HEAD~1..HEAD", "HEAD"],
        &["rev-parse", "--verify", "HEAD", "HEAD~1..HEAD"],
        &["rev-parse", "--verify", "HEAD~1..HEAD", "HEAD", "HEAD~1"],
        &["rev-parse", "--short", "..HEAD", "HEAD"],
        &["rev-parse", "--short", "..HEAD", "@"],
        &["rev-parse", "--short=", "--symbolic", "@", "HEAD^@", "..HEAD"],
        &["rev-parse", "--verify", "--default", "HEAD", "HEAD~1..HEAD"],
    ];
    for args in cases {
        assert_eq!(run(BIN, &root, args), run(stock, &root, args), "{args:?}");
    }
    let _ = std::fs::remove_dir_all(&root);
}
