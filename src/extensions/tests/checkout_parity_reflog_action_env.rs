//! `GIT_REFLOG_ACTION` and the `HEAD` reflog line of a branch switch.
//!
//! `update_refs_for_switch()` writes `checkout: moving from <old> to <new>` only when
//! `GIT_REFLOG_ACTION` is unset; when it is set the environment value is the entire message.
//! zvcs applied that to `--orphan` alone and wrote the long form everywhere else.

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
    let dir = std::env::temp_dir().join(format!("zvcs-reflogaction-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    run(stock, &dir, None, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    run(stock, &dir, None, &["add", "."]);
    run(stock, &dir, None, &["commit", "-qm", "one"]);
    run(stock, &dir, None, &["branch", "other"]);
    dir
}

#[test]
fn the_environment_value_is_the_whole_message() {
    let Some(stock) = stock_git() else { return };
    let cases: [&[&str]; 8] = [
        &["switch", "-c", "new"],
        &["checkout", "-b", "new"],
        &["checkout", "other"],
        &["switch", "other"],
        &["checkout", "--detach"],
        &["switch", "-C", "other"],
        &["checkout", "-B", "other"],
        &["checkout", "--orphan", "fresh"],
    ];
    for action in [Some("custom action"), None] {
        for args in cases {
            let mut seen = Vec::new();
            for (who, bin) in [("stock", stock), ("zvcs", BIN)] {
                let dir = fixture(who, stock);
                let result = run(bin, &dir, action, args);
                let log = run(stock, &dir, None, &["reflog", "show", "--format=%gs", "HEAD"]).0;
                let _ = std::fs::remove_dir_all(&dir);
                seen.push((result, log));
            }
            assert_eq!(seen[1], seen[0], "action {action:?} args {args:?}");
        }
    }
}
