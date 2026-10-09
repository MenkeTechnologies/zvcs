//! `git mv <dir> <dest>` when a tracked file under `<dir>` is gone from the work tree.
//!
//! A directory source is expanded into its index entries (builtin/mv.c:394-410), and each
//! expansion goes through the checking loop like a source of its own: `lstat()` failing on
//! a file the index lists, without `skip-worktree` to explain it, is `bad source, source=<file>,
//! destination=<file moved>`. `-k` drops that entry only. zvcs checked the directory and moved
//! the entries anyway.

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
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code(),
    )
}

fn fixture(tag: &str, stock: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-mvmissing-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let dir = dir.canonicalize().unwrap();
    std::fs::write(dir.join("src/lib.rs"), "lib\n").unwrap();
    std::fs::write(dir.join("src/mod.rs"), "mod\n").unwrap();
    std::fs::write(dir.join("README.md"), "r\n").unwrap();
    run(stock, &dir, &["init", "-q", "-b", "main"]);
    run(stock, &dir, &["add", "."]);
    assert_eq!(run(stock, &dir, &["commit", "-qm", "one"]).2, Some(0));
    std::fs::remove_file(dir.join("src/lib.rs")).unwrap();
    dir
}

#[test]
fn a_tracked_file_missing_under_a_moved_directory_is_a_bad_source() {
    let Some(stock) = stock_git() else { return };
    let cases: [&[&str]; 6] = [
        &["mv", "src", "dest"],
        &["mv", "-n", "src", "dest"],
        &["mv", "-k", "src", "dest"],
        &["mv", "-k", "-n", "src", "dest"],
        &["mv", "-f", "src", "dest"],
        &["mv", "src", "README.md"],
    ];
    for args in cases {
        let mut seen = Vec::new();
        for (who, bin) in [("stock", stock), ("zvcs", BIN)] {
            let dir = fixture(who, stock);
            let result = run(bin, &dir, args);
            let after = (
                run(stock, &dir, &["status", "--porcelain"]).0,
                run(stock, &dir, &["ls-files", "--stage"]).0,
            );
            let _ = std::fs::remove_dir_all(&dir);
            seen.push((result, after));
        }
        assert_eq!(seen[1], seen[0], "args {args:?}");
    }
}
