//! Three `git restore` divergences, each checked against stock on twin fixtures.
//!
//! * `cmd_restore()` parses with neither `PARSE_OPT_KEEP_DASHDASH` (only a
//!   command taking both refs and paths sets it, builtin/checkout.c:1911-1912)
//!   nor `PARSE_OPT_KEEP_UNKNOWN_OPT`, so `--end-of-options` ends the options
//!   and is dropped the way `--` is (parse-options.c:1113-1124). zvcs refused it
//!   as an unknown option at 129.
//! * `restore -m` on an unmerged path merges in `merge.conflictStyle` when no
//!   `--conflict` is given: `checkout_merged()` passes `opts->conflict_style`
//!   (-1) to `ll_merge()`, which falls back to `git_xmerge_style`
//!   (builtin/checkout.c:350, merge-ll.c:135-138). zvcs always wrote the
//!   two-way `merge` markers.
//! * `--source` is read with `repo_get_oid_mb()` (builtin/checkout.c:2026-2027),
//!   so `<a>...<b>` names the merge base of the two sides, in `-p` too; zvcs
//!   refused it as unresolvable.

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
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_EDITOR", "true")
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

fn repo_dir(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-restore-eoo-cs-mb-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join("repo");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(&dir).unwrap()
}

fn stock_run(stock: &str, dir: &Path, args: &[&str]) -> i32 {
    git(stock, dir, &[&["-c", "maintenance.auto=false"], args].concat()).0
}

/// One commit holding `a.txt` and `b.txt`, then `a.txt` modified in the
/// worktree and `b.txt` modified and staged.
fn dirty(stock: &str, label: &str) -> PathBuf {
    let dir = repo_dir(label);
    let ok = |args: &[&str]| assert_eq!(stock_run(stock, &dir, args), 0, "{args:?}");
    ok(&["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    std::fs::write(dir.join("b.txt"), "b\n").unwrap();
    ok(&["add", "a.txt", "b.txt"]);
    ok(&["commit", "-q", "-m", "one"]);
    std::fs::write(dir.join("a.txt"), "dirty\n").unwrap();
    std::fs::write(dir.join("b.txt"), "staged\n").unwrap();
    ok(&["add", "b.txt"]);
    dir
}

/// `c.txt` changed on both sides of a merge and left conflicted.
fn conflicted(stock: &str, label: &str) -> PathBuf {
    let dir = repo_dir(label);
    let ok = |args: &[&str]| assert_eq!(stock_run(stock, &dir, args), 0, "{args:?}");
    ok(&["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("c.txt"), "1\n2\n3\n").unwrap();
    ok(&["add", "c.txt"]);
    ok(&["commit", "-q", "-m", "base"]);
    ok(&["checkout", "-q", "-b", "other"]);
    std::fs::write(dir.join("c.txt"), "1\nTHEIRS\n3\n").unwrap();
    ok(&["commit", "-q", "-am", "theirs"]);
    ok(&["checkout", "-q", "main"]);
    std::fs::write(dir.join("c.txt"), "1\nOURS\n3\n").unwrap();
    ok(&["commit", "-q", "-am", "ours"]);
    assert_eq!(stock_run(stock, &dir, &["merge", "-q", "other"]), 1);
    dir
}

/// `a.txt` at `base`, then changed on `main` and on a `side` branch forked
/// from `base`.
fn forked(stock: &str, label: &str) -> PathBuf {
    let dir = repo_dir(label);
    let ok = |args: &[&str]| assert_eq!(stock_run(stock, &dir, args), 0, "{args:?}");
    ok(&["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("a.txt"), "base\n").unwrap();
    ok(&["add", "a.txt"]);
    ok(&["commit", "-q", "-m", "base"]);
    ok(&["checkout", "-q", "-b", "side"]);
    std::fs::write(dir.join("a.txt"), "side\n").unwrap();
    ok(&["commit", "-q", "-am", "side"]);
    ok(&["checkout", "-q", "main"]);
    std::fs::write(dir.join("a.txt"), "main\n").unwrap();
    ok(&["commit", "-q", "-am", "main"]);
    dir
}

fn same(label: &str, args: &[&str], make: fn(&str, &str) -> PathBuf) {
    let Some(stock) = stock_git() else { return };
    let mut seen = Vec::new();
    for (bin, side) in [(ZVCS, "zvcs"), (stock, "stock")] {
        let dir = make(stock, &format!("{label}-{side}"));
        let out = git(bin, &dir, args);
        let status = git(stock, &dir, &["status", "--porcelain=v2"]);
        let files: Vec<String> = ["a.txt", "b.txt", "c.txt"]
            .iter()
            .map(|f| std::fs::read_to_string(dir.join(f)).unwrap_or_default())
            .collect();
        seen.push((out, status, files));
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }
    assert_eq!(seen[0], seen[1], "git {args:?}: left is zvcs, right is stock");
}

#[test]
fn end_of_options_ends_the_options() {
    same("plain", &["restore", "--end-of-options", "a.txt"], dirty);
    same("staged", &["restore", "--staged", "--end-of-options", "b.txt"], dirty);
    same("dashdash-is-a-path", &["restore", "--end-of-options", "--", "a.txt"], dirty);
    same("clump-is-a-path", &["restore", "--end-of-options", "-SW", "b.txt"], dirty);
}

#[test]
fn merge_without_conflict_uses_the_configured_style() {
    same("cfg-diff3", &["-c", "merge.conflictStyle=diff3", "restore", "-m", "c.txt"], conflicted);
    same("cfg-zdiff3", &["-c", "merge.conflictStyle=zdiff3", "restore", "--merge", "."], conflicted);
    same("opt-wins", &["-c", "merge.conflictStyle=diff3", "restore", "--conflict=merge", "c.txt"], conflicted);
}

#[test]
fn source_is_read_with_the_merge_base_form() {
    same("mb", &["restore", "--source=main...side", "a.txt"], forked);
    same("mb-staged", &["restore", "-s", "...side", "--staged", "--worktree", "a.txt"], forked);
    same("mb-patch", &["restore", "-p", "--source=main...side", "a.txt"], forked);
    same("mb-missing", &["restore", "--source=main...nosuch", "a.txt"], forked);
}
