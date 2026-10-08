//! `git shortlog` against stock git: revision operands are resolved before the
//! `--grep`/`--author` patterns are compiled.
//!
//! `setup_revisions()` walks the operands first and calls `compile_grep_patterns()`
//! after them, so an unknown revision is reported ahead of a pattern that does not
//! compile.
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
fn an_unknown_revision_beats_an_uncompilable_pattern() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = std::env::temp_dir().join(format!("zvcs-shortlog-order-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    run(stock, &root, &["init", "-q", "-b", "main"]);
    run(stock, &root, &["commit", "-q", "--allow-empty", "-m", "one"]);

    let cases: &[&[&str]] = &[
        &["shortlog", "-E", "--author=(", "nonesuch"],
        &["shortlog", "-E", "--grep=(", "nonesuch..HEAD"],
        &["shortlog", "-E", "--author=(", "HEAD"],
        &["shortlog", "-E", "--author=(", "-s", "HEAD", "nonesuch"],
        &["shortlog", "--author=\\(", "nonesuch"],
        &["shortlog", "-E", "--author=(", "--", "nonesuch"],
    ];
    for args in cases {
        assert_eq!(run(BIN, &root, args), run(stock, &root, args), "{args:?}");
    }
    let _ = std::fs::remove_dir_all(&root);
}
