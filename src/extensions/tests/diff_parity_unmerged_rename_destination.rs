//! An unmerged pair as a rename destination.
//!
//! `diff_unmerge()` queues a conflicted path as a pair flagged `is_unmerged`, and
//! `run_diff_files()` gives its post-image the worktree file's mode
//! (diff-lib.c:222-224). `diffcore_rename()` registers every pair whose pre-image is
//! invalid and post-image valid as a destination (diffcore-rename.c:1420-1433), the
//! unmerged one included, so once any source exists the worktree file is hashed —
//! `--raw` prints its id on the `U` record — and a deleted file with the same content
//! becomes its source: the deletion drops out and the record turns into `U100` named
//! after the source. zvcs kept unmerged pairs out of the rename pass and printed
//! zeros.

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
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
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

/// `aa.txt` and `conflict.txt` conflict (`aa.txt` executable on the other side);
/// `gone.txt` is deleted from the worktree, which makes it a rename source. With
/// `copy_gone`, `conflict.txt` first takes `gone.txt`'s exact content.
fn fixture(tag: &str, stock: &str, copy_gone: bool) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-unmergedrename-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    run(stock, &dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("conflict.txt"), "base\nline2\n").unwrap();
    std::fs::write(dir.join("aa.txt"), "aa\n").unwrap();
    std::fs::write(dir.join("gone.txt"), "gone content here\nmore\n").unwrap();
    run(stock, &dir, &["add", "."]);
    run(stock, &dir, &["commit", "-qm", "base"]);
    run(stock, &dir, &["checkout", "-qb", "side"]);
    std::fs::write(dir.join("conflict.txt"), "side\nline2\n").unwrap();
    std::fs::write(dir.join("aa.txt"), "aaside\n").unwrap();
    run(stock, &dir, &["update-index", "--chmod=+x", "aa.txt"]);
    run(stock, &dir, &["commit", "-qam", "side"]);
    run(stock, &dir, &["checkout", "-q", "main"]);
    std::fs::write(dir.join("conflict.txt"), "main\nline2\n").unwrap();
    std::fs::write(dir.join("aa.txt"), "aamain\n").unwrap();
    run(stock, &dir, &["commit", "-qam", "main"]);
    assert_eq!(run(stock, &dir, &["merge", "-q", "side"]).2, Some(1));
    if copy_gone {
        std::fs::copy(dir.join("gone.txt"), dir.join("conflict.txt")).unwrap();
    }
    std::fs::remove_file(dir.join("gone.txt")).unwrap();
    dir
}

const DIFFS: [&[&str]; 16] = [
    &["diff", "--raw"],
    &["diff", "-M", "--raw"],
    &["diff", "--no-renames", "--raw"],
    &["diff", "-C", "--raw"],
    &["diff", "-C", "--find-copies-harder", "--raw"],
    &["diff", "--raw", "-z"],
    &["diff", "-M", "--name-status"],
    &["diff", "--name-only"],
    &["diff", "--stat"],
    &["diff", "--numstat"],
    &["diff", "--summary"],
    &["diff", "--base"],
    &["diff", "--base", "--raw"],
    &["diff", "--base", "--compact-summary"],
    &["diff", "--cached", "--raw"],
    &["diff", "-C", "--cached", "--raw"],
];

#[test]
fn an_unmerged_pair_is_hashed_and_matched_as_a_rename_destination() {
    let Some(stock) = stock_git() else { return };
    for copy_gone in [false, true] {
        let mut seen = Vec::new();
        for (who, bin) in [("stock", stock), ("zvcs", BIN)] {
            let dir = fixture(&format!("{copy_gone}-{who}"), stock, copy_gone);
            seen.push(DIFFS.iter().map(|args| run(bin, &dir, args)).collect::<Vec<_>>());
            let _ = std::fs::remove_dir_all(&dir);
        }
        for (i, args) in DIFFS.iter().enumerate() {
            assert_eq!(seen[1][i], seen[0][i], "copy_gone={copy_gone}: {args:?}");
        }
        // The `U` record carries the hashed worktree id, never all-zero.
        assert!(!seen[0][0].0.lines().next().unwrap_or("").contains("0000000000 U"), "{:?}", seen[0][0]);
    }
}
