//! `git reset <operand>` against stock git: a leading operand that is no revision.
//!
//! `verify_filename()` lets a token through when `looks_like_pathspec()` says it carries an
//! unescaped glob character or long-form magic, and only otherwise probes the work tree, so
//! `git reset '*.rs'` is a pathspec that matches nothing while `git reset nope.rs` is
//! `ambiguous argument`.
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
fn a_wildcard_operand_is_a_pathspec_and_a_missing_name_is_not() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = std::env::temp_dir().join(format!("zvcs-reset-wildcard-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    run(stock, &root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join("a"), "one\n").unwrap();
    run(stock, &root, &["add", "a"]);
    run(stock, &root, &["commit", "-qm", "one"]);

    let cases: &[&[&str]] = &[
        &["reset", "*.rs"],
        &["reset", "-q", "*.rs"],
        &["reset", "[xy]"],
        &["reset", "?.rs"],
        &["reset", ":(glob)*.rs"],
        &["reset", "*.rs", "a"],
        &["reset", "a*"],
        &["reset", "nope.rs"],
        &["reset", ":/nope"],
        &["reset", "nope.rs", "*.rs"],
        &["reset", "--soft", "*.rs"],
        &["reset", "HEAD", "*.rs"],
    ];
    for args in cases {
        assert_eq!(run(BIN, &root, args), run(stock, &root, args), "{args:?}");
    }
    let _ = std::fs::remove_dir_all(&root);
}
