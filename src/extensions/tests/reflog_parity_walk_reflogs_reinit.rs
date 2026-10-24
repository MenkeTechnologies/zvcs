//! `-g`/`--walk-reflogs` runs `init_reflog_walk()` (revision.c:2459-2460), a bare
//! `CALLOC_ARRAY(*info, 1)` (reflog-walk.c:132-136). `cmd_log_reflog()` has already run it once,
//! so a `-g` on the `git reflog` command line discards every reflog queued before it:
//! `git reflog --all --walk-reflogs` prints nothing, and only what follows the last `-g` is
//! walked. zvcs kept the position of the first `-g` and walked `--all`.

use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn git(bin: &str, dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir.parent().unwrap())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    (
        out.status.code().expect("no signal"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Two commits on `main` and a branch `other`, built by stock git.
fn fixture(stock: &str, label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-reflog-reinit-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join("repo");
    std::fs::create_dir_all(&dir).unwrap();
    let dir = std::fs::canonicalize(&dir).unwrap();
    let run = |args: &[&str]| {
        let out = git(stock, &dir, args);
        assert_eq!(out.0, 0, "{args:?}: {out:?}");
    };
    run(&["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("f"), "one\n").unwrap();
    run(&["add", "f"]);
    run(&["commit", "-q", "-m", "one"]);
    std::fs::write(dir.join("f"), "two\n").unwrap();
    run(&["commit", "-q", "-am", "two"]);
    run(&["branch", "other"]);
    dir
}

fn same(label: &str, args: &[&str]) {
    let Some(stock) = stock_git() else { return };
    let mut seen = Vec::new();
    for (bin, side) in [(stock, "stock"), (ZVCS, "zvcs")] {
        let dir = fixture(stock, &format!("{label}-{side}"));
        seen.push(git(bin, &dir, args));
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }
    assert_eq!(seen[1], seen[0], "git {args:?}: left is zvcs, right is stock");
}

#[test]
fn walk_reflogs_after_all_discards_the_queued_reflogs() {
    same("all-g", &["reflog", "--all", "--walk-reflogs"]);
    same("all-g-name-status", &["reflog", "--all", "--name-status", "--patch", "--walk-reflogs", "--no-abbrev-commit"]);
    same("rev-g", &["reflog", "main", "-g"]);
}

#[test]
fn only_what_follows_the_last_g_is_walked() {
    same("g-all", &["reflog", "--walk-reflogs", "--all"]);
    same("g-main-g-head", &["reflog", "-g", "main", "-g", "HEAD"]);
    same("log-g-main-g-other", &["log", "--oneline", "-g", "main", "-g", "other"]);
}
