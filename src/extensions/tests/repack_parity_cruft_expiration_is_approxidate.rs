//! `git repack --cruft-expiration=<date>` hands the string to `pack-objects`, whose
//! option callback reads it with `approxidate()` (builtin/pack-objects.c), not with
//! `parse_expiry_date()`. A string approxidate cannot read is not "never expire": it
//! comes out as the current time, so every unreachable object is older than the
//! cutoff, none is a traversal tip, and no cruft pack is written. The words `never`
//! and `now` keep their meanings (`never` keeps everything, `now` expires it all).
//!
//! zvcs mapped an unreadable string to 0, i.e. "never", and wrote a cruft pack that
//! git does not.

use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn git(bin: &str, dir: &Path, args: &[&str]) -> (i32, String) {
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
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)),
    )
}

/// Two commits, then `reset --hard` back to the first: the second, its tree and its blob
/// are unreachable loose objects whose mtime is "just now".
fn fixture(bin: &str, label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-cruft-exp-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join("repo");
    std::fs::create_dir_all(&dir).unwrap();
    let dir = std::fs::canonicalize(&dir).unwrap();
    git(bin, &dir, &["init", "-q", "-b", "main"]);
    for (name, body) in [("a", "one\n"), ("b", "two\n")] {
        std::fs::write(dir.join(name), body).unwrap();
        git(bin, &dir, &["add", name]);
        git(bin, &dir, &["commit", "-q", "-m", name]);
    }
    git(bin, &dir, &["reset", "-q", "--hard", "HEAD~1"]);
    git(bin, &dir, &["reflog", "expire", "--expire=now", "--all"]);
    dir
}

/// The pack directory as sorted `<suffix>:<size>`, which is the same for both gits whenever the
/// same objects went into the same packs.
fn packs(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir.join(".git/objects/pack"))
        .unwrap()
        .filter_map(|e| {
            let e = e.unwrap();
            let name = e.file_name().into_string().unwrap();
            let suffix = name.rsplit('.').next()?.to_owned();
            Some(format!("{suffix}:{}", e.metadata().unwrap().len()))
        })
        .collect();
    out.sort();
    out
}

fn same(label: &str, expiration: &str) {
    let Some(stock) = stock_git() else { return };
    let mut seen = Vec::new();
    for (bin, side) in [(stock, "stock"), (ZVCS, "zvcs")] {
        let dir = fixture(bin, &format!("{label}-{side}"));
        let run = git(bin, &dir, &["repack", "-q", "--cruft", &format!("--cruft-expiration={expiration}")]);
        seen.push((run, packs(&dir)));
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }
    assert_eq!(seen[1], seen[0], "--cruft-expiration={expiration:?}: left is zvcs, right is stock");
}

#[test]
fn an_unreadable_date_is_now_so_nothing_is_kept() {
    same("bogus", "bogus");
    same("tab", "\t");
    same("empty", "");
}

#[test]
fn the_words_and_real_dates_keep_their_meaning() {
    same("never", "never");
    same("false", "false");
    same("now", "now");
    same("all", "all");
    same("ago", "2.weeks.ago");
    same("future", "2999-01-01");
}
