//! `cherry-pick --continue` / `revert --continue` over an index that still holds conflict stages.
//!
//! With no `CHERRY_PICK_HEAD` to resume, `sequencer_continue()` goes to
//! `index_differs_from(HEAD)` and `error_dirty_index()`, whose first move is
//! `repo_read_index_unmerged()`: stages left in the index make it
//! `error_resolve_conflict()` (`Cherry-picking is not possible because you have unmerged
//! files.`), not `your local changes would be overwritten`. zvcs only had the second.

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
        // Fixed stamps: the stock and zvcs fixtures are built one after the other, and ids that
        // hash the wall clock differ whenever a second boundary falls between them.
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("GIT_EDITOR", "true")
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

/// Three commits on `main` that each rewrite `f.txt`; the checkout sits on `side`, one commit
/// behind the middle of them, so picking all three with `-n` conflicts on the second.
fn fixture(tag: &str, stock: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-pickcont-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    run(stock, &dir, &["init", "-q", "-b", "main"]);
    for (i, text) in ["a\nb\nc\n", "a\nB\nc\n", "a\nBB\nc\nd\n", "x\nBBB\nc\nd\n"].iter().enumerate() {
        std::fs::write(dir.join("f.txt"), text).unwrap();
        run(stock, &dir, &["add", "."]);
        run(stock, &dir, &["commit", "-qm", &format!("c{i}")]);
    }
    run(stock, &dir, &["checkout", "-q", "-b", "side", "main~3"]);
    std::fs::write(dir.join("f.txt"), "a\nside\nc\n").unwrap();
    run(stock, &dir, &["commit", "-qam", "side"]);
    dir
}

#[test]
fn unmerged_stages_are_reported_as_such() {
    let Some(stock) = stock_git() else { return };
    let cases: [(&[&str], &[&str]); 4] = [
        (&["cherry-pick", "-n", "main~1", "main"], &["cherry-pick", "--continue"]),
        (&["cherry-pick", "--no-commit", "main~2", "main~1", "main"], &["cherry-pick", "--continue"]),
        (&["revert", "-n", "main~1", "main~2"], &["revert", "--continue"]),
        (&["cherry-pick", "-n", "main~1", "main"], &["cherry-pick", "--skip"]),
    ];
    for (start, resume) in cases {
        let mut seen = Vec::new();
        for (who, bin) in [("stock", stock), ("zvcs", BIN)] {
            let dir = fixture(who, stock);
            let first = run(bin, &dir, start);
            let result = run(bin, &dir, resume);
            let after = run(stock, &dir, &["status", "--porcelain"]).0;
            let _ = std::fs::remove_dir_all(&dir);
            seen.push((first.2, result, after));
        }
        assert_eq!(seen[1], seen[0], "{start:?} then {resume:?}");
    }
}
