//! A rebase pick that conflicts, with a `rerere.autoUpdate` git refuses.
//!
//! `do_pick_commit()` prints the `error: could not apply` line and the advice and then runs
//! `repo_rerere()`; only afterwards does `pick_commits()` reach `error_with_patch()`, which
//! writes `stopped-sha`, `REBASE_HEAD`, `patch` and `message`. A value rerere's config
//! callback dies on therefore ends the command at 128 with none of those on disk. zvcs wrote
//! them first.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, envs: &[(&str, &str)], args: &[&str]) -> (String, String, Option<i32>) {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "a")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "a")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_EDITOR", "true")
        .env("HOME", dir)
        .env("LC_ALL", "C");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code(),
    )
}

fn fixture(tag: &str, stock: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-rebrerere-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    run(stock, &dir, &[], &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("base.txt"), "base\n").unwrap();
    run(stock, &dir, &[], &["add", "."]);
    run(stock, &dir, &[], &["commit", "-qm", "base"]);
    run(stock, &dir, &[], &["checkout", "-q", "-b", "theirs"]);
    std::fs::write(dir.join("conflict.txt"), "theirs\n").unwrap();
    run(stock, &dir, &[], &["add", "."]);
    run(stock, &dir, &[], &["commit", "-qm", "theirs"]);
    run(stock, &dir, &[], &["checkout", "-q", "main"]);
    std::fs::write(dir.join("conflict.txt"), "ours\n").unwrap();
    run(stock, &dir, &[], &["add", "."]);
    run(stock, &dir, &[], &["commit", "-qm", "ours"]);
    dir
}

/// Every file and directory name below `.git` that belongs to the rebase, with the contents of
/// the small ones, sorted.
fn state(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let git = dir.join(".git");
    for name in ["REBASE_HEAD", "MERGE_MSG", "ORIG_HEAD", "AUTO_MERGE"] {
        out.push(format!("{name}: {}", git.join(name).exists()));
    }
    let mut files: Vec<String> = std::fs::read_dir(git.join("rebase-merge"))
        .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    files.sort();
    out.push(files.join(","));
    out
}

#[test]
fn a_dying_rerere_leaves_the_stop_files_unwritten() {
    let Some(stock) = stock_git() else { return };
    let cases: [&[(&str, &str)]; 2] = [
        &[("GIT_CONFIG_COUNT", "1"), ("GIT_CONFIG_KEY_0", "rerere.autoUpdate"), ("GIT_CONFIG_VALUE_0", "warn")],
        &[],
    ];
    for env in cases {
        let mut seen = Vec::new();
        for (who, bin) in [("stock", stock), ("zvcs", BIN)] {
            let dir = fixture(who, stock);
            let result = run(bin, &dir, env, &["rebase", "theirs"]);
            let left = state(&dir);
            let _ = std::fs::remove_dir_all(&dir);
            seen.push((result, left));
        }
        assert_eq!(seen[1], seen[0], "env {env:?}");
    }
}
