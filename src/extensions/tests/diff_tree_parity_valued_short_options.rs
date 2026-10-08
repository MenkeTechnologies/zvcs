//! `-l<num>` (`OPT_INTEGER`) and `-O<file>` (`OPT_FILENAME`) require a value, glued on or
//! taken from the next word. `diff-tree` split a glued `-l100` into `-l` `100` and then read
//! `100` as a tree-ish (`fatal: ambiguous argument '100'`), and `diff-pairs` — where a routed
//! run replays the option — refused the `k`/`m`/`g` suffix parse-options accepts.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "a")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "a")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("LC_ALL", "C")
        .output()
        .expect("run git");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

/// Two commits; the second renames a file with a small edit, so rename detection has
/// work to do and `-l` has something to limit.
fn fixture(bin: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "zvcs-difftree-valued-{}-{}",
        std::process::id(),
        if bin == BIN { "zvcs" } else { "stock" }
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    run(bin, &dir, &["init", "-q", "-b", "main", "."]);
    let body: String = (0..20).map(|n| format!("line {n}\n")).collect();
    std::fs::write(dir.join("old.txt"), &body).unwrap();
    run(bin, &dir, &["add", "old.txt"]);
    run(bin, &dir, &["commit", "-q", "-m", "one"]);
    run(bin, &dir, &["mv", "old.txt", "new.txt"]);
    std::fs::write(dir.join("new.txt"), body + "tail\n").unwrap();
    run(bin, &dir, &["add", "new.txt"]);
    run(bin, &dir, &["commit", "-q", "-m", "two"]);
    dir
}

#[test]
fn glued_and_separated_values_match_stock() {
    let Some(stock) = stock_git() else { return };
    let (s, z) = (fixture(stock), fixture(BIN));
    let vectors: &[&[&str]] = &[
        &["diff-tree", "-r", "-M", "-l100", "HEAD~1", "HEAD"],
        &["diff-tree", "-r", "-M", "-l", "100", "HEAD~1", "HEAD"],
        &["diff-tree", "-r", "-M", "-l1", "HEAD~1", "HEAD"],
        &["diff-tree", "-r", "-M", "-l", "1k", "HEAD~1", "HEAD"],
        &["diff-tree", "-p", "-M", "-l1k", "HEAD~1", "HEAD"],
        &["diff-tree", "-r", "-lfoo", "HEAD~1", "HEAD"],
        &["diff-tree", "-r", "-l", "foo", "HEAD~1", "HEAD"],
        &["diff-tree", "-r", "-l"],
        &["diff-tree", "-r", "-O/dev/null", "HEAD~1", "HEAD"],
        &["diff-tree", "-r", "-O", "/dev/null", "HEAD~1", "HEAD"],
        &["diff-tree", "-r", "-O", "HEAD~1", "HEAD"],
        &["diff-tree", "-r", "-O"],
    ];
    for args in vectors {
        assert_eq!(run(BIN, &z, args), run(stock, &s, args), "{args:?}");
    }
    let _ = (std::fs::remove_dir_all(s), std::fs::remove_dir_all(z));
}
