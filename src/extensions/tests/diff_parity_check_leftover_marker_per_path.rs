//! `--check` over an added line that is a conflict marker.
//!
//! `checkdiff_consume()` (diff.c:3551-3556) reports `<path>:<n>: leftover conflict
//! marker` ahead of the whitespace check of the same line, at the
//! `conflict-marker-size` gitattribute `builtin_checkdiff()` reads for the pair's
//! `attr_path` (diff.c:4276-4277), and its `\` arm reports an incomplete last line
//! when the `whitespace` rule asks for `incomplete-line` (diff.c:3576-3586). zvcs's
//! `git diff --check` (and so `log`/`show --check`) had no marker test and no
//! incomplete-line arm, and `diff-files`/`diff-index --check` used a fixed marker size
//! of 7 and `core.whitespace` for every path; `diff-index --check --relative` also
//! printed the unstripped name.

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

/// A commit, then a worktree that adds markers of the default size, a 10-wide
/// marker under `conflict-marker-size=10`, one under a non-numeric size (warned
/// about, then 7), and a final line without a newline under `incomplete-line`.
fn fixture(tag: &str, stock: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-checkmarker-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    let dir = dir.canonicalize().unwrap();
    run(stock, &dir, &["init", "-q", "-b", "main"]);
    for f in ["f.txt", "g.txt", "big.txt", "bad.txt", "sub/m.txt"] {
        std::fs::write(dir.join(f), "a\n").unwrap();
    }
    run(stock, &dir, &["add", "."]);
    run(stock, &dir, &["commit", "-qm", "base"]);
    std::fs::write(dir.join("f.txt"), "a\n<<<<<<< ours\nb\n=======\n>>>>>>> theirs\n").unwrap();
    std::fs::write(dir.join("g.txt"), "a\ny").unwrap();
    std::fs::write(dir.join("big.txt"), "a\n<<<<<<<<<< x\n<<<<<<< y\n").unwrap();
    std::fs::write(dir.join("bad.txt"), "a\n=======\n").unwrap();
    std::fs::write(dir.join("sub/m.txt"), "a\n|||||||| base\n=====\n=======\n").unwrap();
    std::fs::write(
        dir.join(".gitattributes"),
        "big.txt conflict-marker-size=10\nbad.txt conflict-marker-size=zz\ng.txt whitespace=incomplete-line\n",
    )
    .unwrap();
    std::fs::write(dir.join("sub/.gitattributes"), "m.txt conflict-marker-size=5\n").unwrap();
    dir
}

const WORKTREE: [&[&str]; 9] = [
    &["diff", "--check"],
    &["diff", "-p", "--check"],
    &["diff", "--check", "HEAD"],
    &["diff", "--check", "--exit-code"],
    &["diff", "--check", "--relative=sub"],
    &["diff-files", "--check"],
    &["diff-files", "--check", "--relative=sub"],
    &["diff-index", "--check", "HEAD"],
    &["diff-index", "--check", "--relative=sub", "HEAD"],
];

/// The same file set staged and committed, for the index and history verbs.
const COMMITTED: [&[&str]; 7] = [
    &["diff", "--check", "HEAD^", "HEAD"],
    &["diff", "--cached", "--check", "HEAD^"],
    &["diff-index", "--cached", "--check", "HEAD^"],
    &["diff-tree", "--check", "HEAD^", "HEAD"],
    &["log", "-1", "--check"],
    &["show", "--check"],
    &["show", "--check", "--color=always"],
];

#[test]
fn leftover_markers_and_incomplete_lines_follow_the_path_attributes() {
    let Some(stock) = stock_git() else { return };
    let mut seen = Vec::new();
    for (who, bin) in [("stock", stock), ("zvcs", BIN)] {
        let dir = fixture(who, stock);
        let mut results: Vec<_> = WORKTREE.iter().map(|args| run(bin, &dir, args)).collect();
        run(stock, &dir, &["add", "-A"]);
        run(stock, &dir, &["commit", "-qm", "markers"]);
        results.extend(COMMITTED.iter().map(|args| run(bin, &dir, args)));
        let _ = std::fs::remove_dir_all(&dir);
        seen.push(results);
    }
    for (i, args) in WORKTREE.iter().chain(COMMITTED.iter()).enumerate() {
        assert_eq!(seen[1][i], seen[0][i], "{args:?}");
    }
    // The fixture really exercises the marker and incomplete-line arms.
    assert!(seen[0][0].0.contains("big.txt:2: leftover conflict marker"), "{:?}", seen[0][0]);
    assert!(seen[0][0].0.contains("g.txt:2: no newline at the end of file."), "{:?}", seen[0][0]);
}

/// A conflicted merge: `git diff --check` checks the stage-2 pair against the
/// worktree file `run_diff_files()` queues after the `U` pair, which `run_checkdiff()`
/// skips (diff.c:5097-5100).
#[test]
fn a_conflicted_file_reports_its_markers() {
    let Some(stock) = stock_git() else { return };
    let mut seen = Vec::new();
    let args: [&[&str]; 8] = [
        &["diff", "--check"],
        &["diff", "-p", "--check"],
        &["diff", "--base", "--check"],
        &["diff", "--theirs", "--check"],
        &["diff", "--check", "HEAD"],
        &["diff", "--cc", "--check"],
        &["diff-files", "--check"],
        &["diff-index", "--check", "HEAD"],
    ];
    for who in ["stock", "zvcs"] {
        let bin = if who == "stock" { stock } else { BIN };
        let dir = std::env::temp_dir().join(format!("zvcs-checkconflict-{who}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        run(stock, &dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("conflict.txt"), "base\nline2\n").unwrap();
        run(stock, &dir, &["add", "."]);
        run(stock, &dir, &["commit", "-qm", "base"]);
        run(stock, &dir, &["checkout", "-qb", "side"]);
        std::fs::write(dir.join("conflict.txt"), "side\nline2\n").unwrap();
        run(stock, &dir, &["commit", "-qam", "side"]);
        run(stock, &dir, &["checkout", "-q", "main"]);
        std::fs::write(dir.join("conflict.txt"), "main\nline2\n").unwrap();
        run(stock, &dir, &["commit", "-qam", "main"]);
        assert_eq!(run(stock, &dir, &["merge", "-q", "side"]).2, Some(1));
        seen.push(args.iter().map(|a| run(bin, &dir, a)).collect::<Vec<_>>());
        let _ = std::fs::remove_dir_all(&dir);
    }
    for (i, a) in args.iter().enumerate() {
        assert_eq!(seen[1][i], seen[0][i], "{a:?}");
    }
    assert_eq!(seen[0][0].2, Some(2));
}
