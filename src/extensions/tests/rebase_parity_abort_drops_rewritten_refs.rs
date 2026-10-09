//! `rebase --abort` and `--quit` after a `--rebase-merges` run stopped on a conflict.
//!
//! The run labelled `refs/rewritten/onto` and listed it in `refs-to-delete`;
//! `sequencer_remove_state()` deletes every ref in that list when the state is dropped by
//! `--abort` or `--quit`, as it does at the end of a finished run. zvcs only did so at the
//! end, leaving the scratch ref behind.

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

/// `main` and `theirs` both add `conflict.txt`, so rebasing `main` onto `theirs` stops.
fn stopped(tag: &str, stock: &str, bin: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-rwabort-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    run(stock, &dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("base.txt"), "base\n").unwrap();
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
    assert_eq!(run(bin, &dir, &["rebase", "--rebase-merges", "theirs"]).2, Some(1));
    dir
}

#[test]
fn abort_and_quit_delete_the_scratch_refs() {
    let Some(stock) = stock_git() else { return };
    for finish in ["--abort", "--quit"] {
        let mut seen = Vec::new();
        for (who, bin) in [("stock", stock), ("zvcs", BIN)] {
            let dir = stopped(&format!("{}-{who}", &finish[2..]), stock, bin);
            let before = run(stock, &dir, &["for-each-ref", "--format=%(refname)"]).0;
            let result = run(bin, &dir, &["rebase", finish]);
            let after = run(stock, &dir, &["for-each-ref", "--format=%(refname)"]).0;
            let _ = std::fs::remove_dir_all(&dir);
            seen.push((before, result, after));
        }
        assert_eq!(seen[1], seen[0], "{finish}");
        assert!(!seen[0].2.contains("refs/rewritten/"), "{finish}: {}", seen[0].2);
    }
}
