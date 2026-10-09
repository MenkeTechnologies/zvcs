//! `git filter-branch` judges a clean work tree by the exit status of two children,
//! `git diff-files --quiet` and `git diff-index --cached --quiet HEAD`
//! (`require_clean_work_tree` in git-sh-setup). A child that dies on a config value it
//! refuses (`diff.renameLimit=false`) therefore reads as a difference: each prints its own
//! `fatal:`, the script says the tree has unstaged changes, then — the second child having
//! failed too — that the index has uncommitted ones, and the run ends at 1 before it looks at
//! its arguments. zvcs answered from an in-process status and went on to the revision parse.

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
        .env("FILTER_BRANCH_SQUELCH_WARNING", "1")
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
    let dir = std::env::temp_dir().join(format!("zvcs-fbgate-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    std::fs::write(dir.join("README.md"), "a\n").unwrap();
    run(stock, &dir, &["init", "-q", "-b", "main"]);
    run(stock, &dir, &["add", "."]);
    assert_eq!(run(stock, &dir, &["commit", "-qm", "one"]).2, Some(0));
    dir
}

#[test]
fn a_diff_child_that_dies_on_config_makes_the_tree_dirty_in_both_messages() {
    let Some(stock) = stock_git() else { return };
    let mut all = Vec::new();
    for bin in [stock, BIN] {
        let dir = fixture("cfg", stock);
        let mut results = Vec::new();
        // A clean tree, then one with only a staged change, then one with an unstaged edit.
        for step in 0..3 {
            match step {
                1 => {
                    std::fs::write(dir.join("README.md"), "b\n").unwrap();
                    run(stock, &dir, &["add", "README.md"]);
                }
                2 => std::fs::write(dir.join("README.md"), "c\n").unwrap(),
                _ => {}
            }
            for args in [
                &["-c", "diff.renameLimit=false", "filter-branch", "README.md"][..],
                &["filter-branch", "README.md"],
            ] {
                results.push(run(bin, &dir, args));
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        all.push(results);
    }
    assert_eq!(all[1], all[0]);
}
