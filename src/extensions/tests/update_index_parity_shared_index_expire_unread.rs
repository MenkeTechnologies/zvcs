//! `splitIndex.sharedIndexExpire` is read only when there is an old shared index
//! to judge, checked against stock on twin fixtures.
//!
//! `get_shared_index_expire_date()` (read-cache.c:3186-3207) is called from
//! `should_delete_shared_index()` (read-cache.c:3209-3224), which
//! `clean_shared_index_files()` runs for each `sharedindex.*` other than the one
//! just written (read-cache.c:3226-3253). With no such file the value is never
//! read, so `git -c splitIndex.sharedIndexExpire=bogus update-index
//! --split-index` on a fresh repository succeeds; zvcs read it up front and died
//! at 128.

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

/// A one-file commit, with `split` already split once by stock.
fn fixture(stock: &str, label: &str, split: bool) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-shared-expire-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join("repo");
    std::fs::create_dir_all(&dir).unwrap();
    let dir = std::fs::canonicalize(&dir).unwrap();
    let ok = |args: &[&str]| {
        let args = [&["-c", "maintenance.auto=false"], args].concat();
        assert_eq!(git(stock, &dir, &args).0, 0, "{args:?}");
    };
    ok(&["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("a"), "a\n").unwrap();
    ok(&["add", "a"]);
    ok(&["commit", "-q", "-m", "one"]);
    if split {
        ok(&["update-index", "--split-index"]);
    }
    std::fs::write(dir.join("b"), "b\n").unwrap();
    dir
}

fn same(label: &str, split: bool, args: &[&str]) {
    let Some(stock) = stock_git() else { return };
    let mut seen = Vec::new();
    for (bin, side) in [(ZVCS, "zvcs"), (stock, "stock")] {
        let dir = fixture(stock, &format!("{label}-{side}"), split);
        let out = git(bin, &dir, args);
        let index = git(stock, &dir, &["ls-files", "-s"]);
        let shared = std::fs::read_dir(dir.join(".git"))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("sharedindex."))
            .count();
        seen.push((out, index, shared));
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }
    assert_eq!(seen[0], seen[1], "git {args:?}: left is zvcs, right is stock");
}

const BOGUS: [&str; 2] = ["-c", "splitIndex.sharedIndexExpire=bogus"];

#[test]
fn a_bad_expiry_goes_unread_without_an_old_shared_index() {
    same("fresh", false, &[&BOGUS[..], &["update-index", "--split-index"]].concat());
    same("unsplit", true, &[&BOGUS[..], &["update-index", "--no-split-index"]].concat());
    same("kept-base", true, &[&BOGUS[..], &["-c", "splitIndex.maxPercentChange=100", "update-index", "--add", "b"]].concat());
}

#[test]
fn a_bad_expiry_dies_once_an_old_shared_index_is_judged() {
    same("new-base", true, &[&BOGUS[..], &["-c", "splitIndex.maxPercentChange=0", "update-index", "--add", "b"]].concat());
    same("resplit", true, &[&BOGUS[..], &["update-index", "--split-index"]].concat());
}
