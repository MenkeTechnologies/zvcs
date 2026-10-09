//! `git checkout` with no operand (and `git checkout HEAD`) is a switch to the
//! commit already checked out. `merge_working_tree()` still runs, and its first
//! acts are `refresh_index(REFRESH_QUIET)` and the unmerged-index refusal; only
//! then does `show_local_changes()` list what differs (builtin/checkout.c).
//!
//! zvcs listed from the unrefreshed index, so a file that was rewritten with the
//! same content (new mtime, same bytes) was reported `M` alongside the one that
//! really changed, and a conflicted index went unrefused.

use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn git(bin: &str, dir: &Path, args: &[&str]) -> Run {
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
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    Run {
        code: out.status.code().expect("no signal"),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// A repository with `touched` (same bytes, rewritten after the commit), `changed`
/// (new bytes) and `clean`, all tracked.
fn fixture(bin: &str, label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-co-inplace-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(&root).unwrap().join("repo");
    std::fs::create_dir_all(&root).unwrap();
    git(bin, &root, &["init", "-q", "-b", "main"]);
    for name in ["touched", "changed", "clean"] {
        std::fs::write(root.join(name), format!("{name}\n")).unwrap();
    }
    git(bin, &root, &["add", "."]);
    git(bin, &root, &["commit", "-q", "-m", "one"]);
    std::thread::sleep(std::time::Duration::from_millis(30));
    // Replaced by a copy, as `cp -R` or a restore tool leaves it: same bytes, new inode and times.
    std::fs::write(root.join("touched.tmp"), "touched\n").unwrap();
    std::fs::rename(root.join("touched.tmp"), root.join("touched")).unwrap();
    std::fs::write(root.join("changed"), "changed again\n").unwrap();
    root
}

fn check(tag: &str, args: &[&str]) {
    let Some(stock) = stock_git() else { return };
    let want_dir = fixture(stock, &format!("{tag}-stock"));
    let got_dir = fixture(ZVCS, &format!("{tag}-zvcs"));
    let want = git(stock, &want_dir, args);
    let got = git(ZVCS, &got_dir, args);
    assert_eq!((got.code, &got.stdout, &got.stderr), (want.code, &want.stdout, &want.stderr), "git {args:?}");
    // The refreshed index reaches disk: a stat-only change is no longer a difference.
    let want_files = git(stock, &want_dir, &["diff-files", "--name-only"]);
    let got_files = git(ZVCS, &got_dir, &["diff-files", "--name-only"]);
    assert_eq!(got_files.stdout, want_files.stdout, "diff-files after git {args:?}");
    let _ = std::fs::remove_dir_all(want_dir.parent().unwrap());
    let _ = std::fs::remove_dir_all(got_dir.parent().unwrap());
}

#[test]
fn bare_checkout_lists_only_real_changes() {
    check("bare", &["checkout"]);
}

#[test]
fn checkout_head_lists_only_real_changes() {
    check("head", &["checkout", "HEAD"]);
}

#[test]
fn bare_checkout_refuses_an_unmerged_index() {
    let Some(stock) = stock_git() else { return };
    let mut results = Vec::new();
    for (bin, label) in [(stock, "stock-unmerged"), (ZVCS, "zvcs-unmerged")] {
        let dir = fixture(bin, label);
        git(bin, &dir, &["checkout", "-q", "-f", "."]);
        git(bin, &dir, &["checkout", "-q", "-b", "side"]);
        std::fs::write(dir.join("clean"), "side\n").unwrap();
        git(bin, &dir, &["commit", "-q", "-am", "side"]);
        git(bin, &dir, &["checkout", "-q", "main"]);
        std::fs::write(dir.join("clean"), "main\n").unwrap();
        git(bin, &dir, &["commit", "-q", "-am", "main"]);
        git(bin, &dir, &["merge", "-q", "side"]);
        let run = git(bin, &dir, &["checkout"]);
        results.push((run.code, run.stdout, run.stderr));
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }
    assert_eq!(results[1], results[0], "left is zvcs, right is stock");
}
