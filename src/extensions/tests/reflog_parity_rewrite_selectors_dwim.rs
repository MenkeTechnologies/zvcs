//! Three `git reflog` divergences, each checked against stock on twin fixtures.
//!
//! * `expire --rewrite`: `expire_reflog_ent()` swaps the entry's old id for
//!   `last_kept_oid` *before* calling the policy (refs/files-backend.c:3505-3508),
//!   so `--expire-unreachable` judges the id the entry will be written with.
//!   zvcs judged the recorded one and pruned entries whose predecessor was gone.
//! * `delete`: `files_reflog_expire()` rewrites the log whenever it is not a dry
//!   run (refs/files-backend.c:3576-3640), even if no entry was dropped, and
//!   `reflog_delete()` reads the ordinal with `strtoul()` into an `int`
//!   (reflog.c:543-547), so `@{-1}` drops nothing where zvcs read it as a date.
//! * `show @`: `add_reflog_for_walk()` falls back to `repo_dwim_log()`, which
//!   substitutes `@` with `HEAD` (reflog-walk.c:189-205, refs.c:839-843); zvcs
//!   printed nothing.

use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn git(bin: &str, dir: &Path, args: &[&str], date: u64) -> (i32, String, String) {
    let date = format!("{date} +0000");
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
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .unwrap();
    (
        out.status.code().expect("no signal"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Four commits on `main`, a reset that strands the last two, a new commit,
/// and a `refs/heads/alias` symref whose own log holds the tab-less entry
/// `symbolic-ref` writes. All dates are in 2023, past the 30-day
/// `gc.reflogExpireUnreachable` default.
fn fixture(stock: &str, label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-reflog-rewrite-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join("repo");
    std::fs::create_dir_all(&dir).unwrap();
    let dir = std::fs::canonicalize(&dir).unwrap();
    let mut date = 1_700_000_000;
    let mut run = |args: &[&str]| {
        date += 60;
        let out = git(stock, &dir, &[&["-c", "maintenance.auto=false"], args].concat(), date);
        assert_eq!(out.0, 0, "{args:?}: {out:?}");
    };
    run(&["init", "-q", "-b", "main"]);
    for n in 1..=4 {
        std::fs::write(dir.join("f"), format!("{n}\n")).unwrap();
        run(&["add", "f"]);
        run(&["commit", "-q", "-m", &format!("c{n}")]);
    }
    run(&["reset", "-q", "--hard", "HEAD~2"]);
    run(&["commit", "-q", "--allow-empty", "-m", "after"]);
    run(&["symbolic-ref", "refs/heads/alias", "refs/heads/main"]);
    dir
}

/// Every log file under `.git/logs`, path and bytes.
fn logs(dir: &Path) -> String {
    let mut out = String::new();
    for name in ["HEAD", "refs/heads/main", "refs/heads/alias"] {
        let body = std::fs::read_to_string(dir.join(".git/logs").join(name)).unwrap_or_default();
        out.push_str(&format!("## {name}\n{body}"));
    }
    out
}

fn same(label: &str, args: &[&str]) {
    let Some(stock) = stock_git() else { return };
    let mut seen = Vec::new();
    for (bin, side) in [(ZVCS, "zvcs"), (stock, "stock")] {
        let dir = fixture(stock, &format!("{label}-{side}"));
        let out = git(bin, &dir, args, 1_700_100_000);
        seen.push((out, logs(&dir)));
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }
    assert_eq!(seen[0], seen[1], "git {args:?}: left is zvcs, right is stock");
}

#[test]
fn expire_rewrite_judges_the_rewritten_old_id() {
    same("rw-never", &["reflog", "expire", "--expire=never", "--rewrite", "--all"]);
    same("rw-unreach", &["reflog", "expire", "--expire-unreachable=now", "--rewrite", "--updateref", "--all"]);
    same("rw-dry", &["reflog", "expire", "--expire-unreachable=now", "--rewrite", "--dry-run", "--verbose", "--all"]);
}

#[test]
fn delete_rewrites_the_log_and_reads_the_ordinal_with_strtoul() {
    same("del-past-end", &["reflog", "delete", "alias@{1}"]);
    same("del-neg", &["reflog", "delete", "--verbose", "HEAD@{-1}"]);
    same("del-space", &["reflog", "delete", "--rewrite", "HEAD@{ 1}"]);
    same("del-plus", &["reflog", "delete", "main@{+2}"]);
}

#[test]
fn show_at_walks_head() {
    same("show-at", &["reflog", "show", "@"]);
    same("log-g-at", &["log", "-g", "--format=%gd %gs", "@"]);
}
