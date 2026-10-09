//! `git diff` over a conflicted path.
//!
//! `run_diff_files()` queues an unmerged path twice: the `U` pair, then the stage-2 (or
//! `-1`/`-2`/`-3`, i.e. `--max-count`) blob against the worktree file. `git diff` arms
//! `skip_stat_unmatch` (builtin/diff.c:525), which drops a pair whose worktree side has no
//! object name when mode and content are identical — so a file that still holds the stage
//! it is compared with adds no `M`. `diff-files` does not arm it and keeps the `M`.
//! zvcs always printed the second pair, and ignored `-<n>`, `-n <n>` and `--max-count=<n>`
//! as the stage selector.

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

/// Both branches add `conflict.txt` differently, so the merge leaves stages 2 and 3.
fn fixture(tag: &str, stock: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-diffstage-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    run(stock, &dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("README.md"), "r\n").unwrap();
    run(stock, &dir, &["add", "."]);
    run(stock, &dir, &["commit", "-qm", "base"]);
    run(stock, &dir, &["checkout", "-q", "-b", "theirs"]);
    std::fs::write(dir.join("conflict.txt"), "theirs\n").unwrap();
    run(stock, &dir, &["add", "."]);
    run(stock, &dir, &["commit", "-qm", "theirs"]);
    run(stock, &dir, &["checkout", "-q", "main"]);
    std::fs::write(dir.join("conflict.txt"), "ours\n").unwrap();
    run(stock, &dir, &["add", "."]);
    run(stock, &dir, &["commit", "-qm", "ours"]);
    assert_eq!(run(stock, &dir, &["merge", "theirs"]).2, Some(1));
    // The merge writes markers; put the stage-2 content back so the worktree equals it.
    std::fs::write(dir.join("conflict.txt"), "ours\n").unwrap();
    dir
}

const DIFFS: [&[&str]; 14] = [
    &["diff", "--name-status"],
    &["diff", "--raw"],
    &["diff", "--stat"],
    &["diff", "-1", "--name-status"],
    &["diff", "-2", "--name-status"],
    &["diff", "-3", "--name-status"],
    &["diff", "-0", "--name-status"],
    &["diff", "-9", "--name-status"],
    &["diff", "-n3", "--name-status"],
    &["diff", "-n", "3", "--name-status"],
    &["diff", "--max-count=3", "--name-status"],
    &["diff", "--max-count=2", "--name-status"],
    &["diff", "--theirs", "--name-status"],
    &["diff-files", "--name-status"],
];

#[test]
fn a_worktree_file_that_still_holds_the_compared_stage_adds_no_modification() {
    let Some(stock) = stock_git() else { return };
    for (what, edit) in [("same", None), ("edited", Some("edited\n"))] {
        let mut seen = Vec::new();
        for (who, bin) in [("stock", stock), ("zvcs", BIN)] {
            let dir = fixture(&format!("{what}-{who}"), stock);
            if let Some(text) = edit {
                std::fs::write(dir.join("conflict.txt"), text).unwrap();
            }
            let results: Vec<_> = DIFFS.iter().map(|args| run(bin, &dir, args)).collect();
            let _ = std::fs::remove_dir_all(&dir);
            seen.push(results);
        }
        for (i, args) in DIFFS.iter().enumerate() {
            assert_eq!(seen[1][i], seen[0][i], "{what}: {args:?}");
        }
    }
}
