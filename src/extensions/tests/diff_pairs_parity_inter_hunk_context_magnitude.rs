//! `git diff-pairs --inter-hunk-context` against stock git.
//!
//! The option is an `OPT_MAGNITUDE`: a `k`/`m`/`g` suffix is read, and a value
//! `parse-options` cannot read is the callback-error `error:` line at 129, not a
//! command-specific message.
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
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (text(&out.stdout), text(&out.stderr), out.status.code())
}

#[test]
fn a_bad_value_is_the_parse_options_error() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = std::env::temp_dir().join(format!("zvcs-dp-inter-hunk-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    run(stock, &root, &["init", "-q", "-b", "main"]);

    let cases: &[&[&str]] = &[
        &["diff-pairs", "--inter-hunk-context=v1"],
        &["diff-pairs", "--inter-hunk-context="],
        &["diff-pairs", "--inter-hunk-context=-1"],
        &["diff-pairs", "--inter-hunk-context=1k"],
        &["diff-pairs", "--inter-hunk-context=3"],
        &["diff-pairs", "--inter-hunk-context", "x"],
        &["diff-pairs", "--inter-hunk-context=99999999999999999999"],
    ];
    for args in cases {
        assert_eq!(run(BIN, &root, args), run(stock, &root, args), "{args:?}");
    }
    let _ = std::fs::remove_dir_all(&root);
}
