//! `git merge --no-verbose` / `--no-quiet`.
//!
//! `OPT__VERBOSITY` is a pair of callbacks, and `verbosity_callback()` puts the level back
//! at 0 when either is negated, so both `--no-` spellings are accepted (and `-q --no-quiet`
//! is no longer quiet). zvcs did not list them as options and read them as commits to merge:
//! `merge: --no-verbose - not something we can merge`.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str]) -> (String, String, Option<i32>) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "a")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "a")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_MERGE_AUTOEDIT", "no")
        .env("HOME", dir)
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code(),
    )
}

fn fixture(tag: &str, stock: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-mergeverb-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    run(stock, &dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    run(stock, &dir, &["add", "."]);
    run(stock, &dir, &["commit", "-qm", "one"]);
    run(stock, &dir, &["checkout", "-q", "-b", "side"]);
    std::fs::write(dir.join("b.txt"), "b\n").unwrap();
    run(stock, &dir, &["add", "."]);
    run(stock, &dir, &["commit", "-qm", "two"]);
    run(stock, &dir, &["checkout", "-q", "main"]);
    std::fs::write(dir.join("c.txt"), "c\n").unwrap();
    run(stock, &dir, &["add", "."]);
    run(stock, &dir, &["commit", "-qm", "three"]);
    dir
}

#[test]
fn the_negated_verbosity_options_are_options() {
    let Some(stock) = stock_git() else { return };
    for args in [
        &["merge", "--no-verbose"][..],
        &["merge", "--no-quiet"],
        &["merge", "-v", "-v", "--no-verbose"],
        &["merge", "--no-verbose", "--no-quiet", "-sours", "--no-edit"],
        &["merge", "--no-verbose", "--no-ff", "--no-edit", "side"],
        &["merge", "-q", "--no-quiet", "--no-ff", "--no-edit", "side"],
        &["merge", "--no-v"],
        &["merge", "--no-q"],
    ] {
        let mut seen = Vec::new();
        for (who, bin) in [("stock", stock), ("zvcs", BIN)] {
            let dir = fixture(who, stock);
            let result = run(bin, &dir, args);
            let after = run(stock, &dir, &["log", "--format=%s", "--all"]).0;
            let _ = std::fs::remove_dir_all(&dir);
            seen.push((result, after));
        }
        assert_eq!(seen[1], seen[0], "args {args:?}");
    }
}
