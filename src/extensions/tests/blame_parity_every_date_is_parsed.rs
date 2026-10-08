//! `git blame --date=<mode>` against stock git: every occurrence is parsed.
//!
//! `--date` is a revision option, so `handle_revision_opt()` runs `parse_date_format()`
//! on each one as it is reached and dies on a format it cannot read, even when a later
//! `--date` would have replaced it. That fatal precedes the usage error blame raises for
//! its operands, which is only reached afterwards.
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
fn a_bad_date_dies_even_when_a_later_one_replaces_it() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = std::env::temp_dir().join(format!("zvcs-blame-dates-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    run(stock, &root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join("a"), "one\n").unwrap();
    run(stock, &root, &["add", "a"]);
    run(stock, &root, &["commit", "-qm", "one"]);

    let cases: &[&[&str]] = &[
        &["blame", "--date=bogus", "--date=iso", "a"],
        &["blame", "--date=iso", "--date=bogus", "a"],
        &["blame", "--date=bogus", "--date=iso", "--", "a", "a"],
        &["blame", "--date", "bogus", "--date=iso", "a"],
        &["blame", "--date=format", "--date=iso", "a"],
        &["blame", "--date=iso", "--date=short", "a"],
        &["annotate", "--date=bogus", "--date=iso", "a"],
    ];
    for args in cases {
        assert_eq!(run(BIN, &root, args), run(stock, &root, args), "{args:?}");
    }
    let _ = std::fs::remove_dir_all(&root);
}
