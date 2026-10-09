//! The reflog line `git reset` writes for `ORIG_HEAD`.
//!
//! `reset_refs()` logs the update as `<action>: updating ORIG_HEAD`, `<action>` being
//! `$GIT_REFLOG_ACTION` or `reset`. zvcs wrote the bare `updating ORIG_HEAD`. A reflog only
//! exists for `ORIG_HEAD` under `core.logAllRefUpdates=always`, which the test turns on.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, action: Option<&str>, args: &[&str]) -> (String, String, Option<i32>) {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "a")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "a")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("HOME", dir)
        .env("LC_ALL", "C");
    if let Some(action) = action {
        cmd.env("GIT_REFLOG_ACTION", action);
    }
    let out = cmd.output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code(),
    )
}

fn fixture(tag: &str, stock: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-resetorig-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    run(stock, &dir, None, &["init", "-q", "-b", "main"]);
    run(stock, &dir, None, &["config", "core.logAllRefUpdates", "always"]);
    for (name, text) in [("one", "1\n"), ("two", "2\n")] {
        std::fs::write(dir.join("a.txt"), text).unwrap();
        run(stock, &dir, None, &["add", "."]);
        run(stock, &dir, None, &["commit", "-qm", name]);
    }
    dir
}

#[test]
fn the_orig_head_entry_carries_the_action() {
    let Some(stock) = stock_git() else { return };
    let cases: [&[&str]; 3] = [
        &["reset", "--soft", "HEAD~1"],
        &["reset", "--mixed", "HEAD~1"],
        &["reset", "--hard", "HEAD~1"],
    ];
    for action in [None, Some("pull --rebase")] {
        for args in cases {
            let mut seen = Vec::new();
            for (who, bin) in [("stock", stock), ("zvcs", BIN)] {
                let dir = fixture(who, stock);
                let result = run(bin, &dir, action, args);
                let orig = std::fs::read_to_string(dir.join(".git/logs/ORIG_HEAD")).unwrap_or_default();
                let orig: Vec<&str> = orig.lines().filter_map(|l| l.split('\t').nth(1)).collect();
                let head = run(stock, &dir, None, &["reflog", "show", "--format=%gs", "HEAD"]).0;
                let _ = std::fs::remove_dir_all(&dir);
                seen.push((result, orig.join("|"), head));
            }
            assert_eq!(seen[1], seen[0], "action {action:?} args {args:?}");
        }
    }
}
