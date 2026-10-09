//! `git reflog show <name>` and `git log -g <name>` print the reflog under the name as typed
//! (`main@{0}`), falling back to the full ref only when none of `read_complete_reflog()`'s four
//! spellings has a log. zvcs asked whether a *loose reflog file* existed for those spellings,
//! which a reftable repository never has, so it always fell back and printed
//! `refs/heads/main@{0}` and `refs/stash@{0}`.

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

/// Two commits on `main`, a branch `other`, a stash entry, in the given ref format.
fn fixture(stock: &str, label: &str, format: &str) -> Option<PathBuf> {
    let root = std::env::temp_dir().join(format!("zvcs-reflog-names-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join("repo");
    std::fs::create_dir_all(&dir).unwrap();
    let dir = std::fs::canonicalize(&dir).unwrap();
    let run = |args: &[&str]| {
        let out = git(stock, &dir, args);
        assert_eq!(out.0, 0, "{args:?}: {out:?}");
    };
    // A stock git too old for reftable cannot build that fixture.
    if git(stock, &dir, &["init", "-q", "-b", "main", &format!("--ref-format={format}")]).0 != 0 {
        return None;
    }
    std::fs::write(dir.join("f"), "one\n").unwrap();
    run(&["add", "f"]);
    run(&["commit", "-q", "-m", "one"]);
    std::fs::write(dir.join("f"), "two\n").unwrap();
    run(&["commit", "-q", "-am", "two"]);
    run(&["branch", "other"]);
    std::fs::write(dir.join("f"), "three\n").unwrap();
    run(&["stash", "push", "-q", "-m", "wip"]);
    Some(dir)
}

fn same(label: &str, format: &str, args: &[&str]) {
    let Some(stock) = stock_git() else { return };
    let mut seen = Vec::new();
    for (bin, side) in [(stock, "stock"), (ZVCS, "zvcs")] {
        let Some(dir) = fixture(stock, &format!("{label}-{format}-{side}"), format) else { return };
        seen.push(git(bin, &dir, args));
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }
    assert_eq!(seen[1], seen[0], "{format}: git {args:?}: left is zvcs, right is stock");
}

#[test]
fn the_name_as_typed_survives_in_either_ref_format() {
    for format in ["files", "reftable"] {
        same("show-main", format, &["reflog", "show", "main"]);
        same("bare-main", format, &["reflog", "main"]);
        same("show-other", format, &["reflog", "show", "other"]);
        same("show-stash", format, &["reflog", "show", "stash"]);
        same("full-name", format, &["reflog", "show", "refs/heads/main"]);
        same("head", format, &["reflog", "show", "HEAD"]);
    }
}

#[test]
fn log_g_prints_the_same_header_name() {
    for format in ["files", "reftable"] {
        same("log-g", format, &["log", "-g", "main"]);
        same("log-g-oneline", format, &["log", "-g", "--oneline", "main"]);
        same("log-g-gd", format, &["log", "-g", "--format=%gd|%gD", "main"]);
        same("log-g-stash", format, &["log", "-g", "--oneline", "stash"]);
    }
}
