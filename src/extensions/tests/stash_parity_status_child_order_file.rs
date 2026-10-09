//! The `git status` child at the end of `stash apply`/`pop`/`branch`.
//!
//! `do_apply_stash()` runs `git status` as a child and ignores how it ends, so a `die()`
//! inside it — here `diff.orderFile` naming a file that cannot be read — prints its
//! `fatal:` line and nothing more: the stash is applied, `pop` still drops the entry, and
//! the command ends at 0. zvcs let the child's death end the command at 128 with the entry
//! kept.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str]) -> (String, String, Option<i32>) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "a")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "a")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("HOME", dir)
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    let root = dir.to_string_lossy();
    let scrub = |b: &[u8]| {
        let text = String::from_utf8_lossy(b).replace(root.as_ref(), "<ROOT>");
        // The dropped entry's id depends on the commit time.
        text.lines()
            .map(|l| match l.find(" (") {
                Some(at) if l.starts_with("Dropped ") => l[..at].to_owned(),
                _ => l.to_owned(),
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    (scrub(&out.stdout), scrub(&out.stderr), out.status.code())
}

fn fixture(tag: &str, stock: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-stashorder-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    run(stock, &dir, &["init", "-q", "-b", "main"]);
    run(stock, &dir, &["add", "."]);
    assert_eq!(run(stock, &dir, &["commit", "-qm", "one"]).2, Some(0));
    std::fs::write(dir.join("a.txt"), "a edited\n").unwrap();
    assert_eq!(run(stock, &dir, &["stash", "push", "-q"]).2, Some(0));
    dir
}

#[test]
fn a_status_child_that_dies_on_the_order_file_does_not_end_the_apply() {
    let Some(stock) = stock_git() else { return };
    for args in [
        &["-c", "diff.orderFile=no-such", "stash", "pop"][..],
        &["-c", "diff.orderFile=no-such", "stash", "apply"],
        &["-c", "diff.orderFile=no-such", "stash", "branch", "side"],
    ] {
        let mut seen = Vec::new();
        for (who, bin) in [("stock", stock), ("zvcs", BIN)] {
            let dir = fixture(who, stock);
            let result = run(bin, &dir, args);
            let after = (
                run(stock, &dir, &["stash", "list"]),
                run(stock, &dir, &["status", "--porcelain"]),
                run(stock, &dir, &["rev-parse", "--abbrev-ref", "HEAD"]),
            );
            let _ = std::fs::remove_dir_all(&dir);
            seen.push((result, after));
        }
        assert_eq!(seen[1], seen[0], "args {args:?}");
    }
}
