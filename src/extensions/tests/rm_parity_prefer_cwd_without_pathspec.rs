//! `git rm` with no pathspec from a subdirectory.
//!
//! `parse_pathspec(&pathspec, 0, PATHSPEC_PREFER_CWD, prefix, argv)` (builtin/rm.c) hands a
//! command run below the top level its own directory as the pathspec when it names none, so
//! `rm` from `src/` is `rm src/` — refused without `-r` as `not removing 'src/' recursively
//! without -r`, and with `-r` it removes everything tracked below. Only at the top level is
//! the pathspec really missing (`No pathspec was given`). zvcs always died with that message.

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
    let dir = std::env::temp_dir().join(format!("zvcs-rmcwd-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src/deep")).unwrap();
    std::fs::create_dir_all(dir.join("untracked")).unwrap();
    let dir = dir.canonicalize().unwrap();
    std::fs::write(dir.join("README.md"), "a\n").unwrap();
    std::fs::write(dir.join("src/lib.rs"), "b\n").unwrap();
    std::fs::write(dir.join("src/deep/x.txt"), "c\n").unwrap();
    std::fs::write(dir.join("untracked/u.txt"), "u\n").unwrap();
    run(stock, &dir, &["init", "-q", "-b", "main"]);
    run(stock, &dir, &["add", "README.md", "src"]);
    assert_eq!(run(stock, &dir, &["commit", "-qm", "one"]).2, Some(0));
    dir
}

#[test]
fn a_subdirectory_is_the_default_pathspec() {
    let Some(stock) = stock_git() else { return };
    let cases: [(&str, &[&str]); 8] = [
        ("src", &["rm"]),
        ("src", &["rm", "-r"]),
        ("src", &["rm", "-n", "-r"]),
        ("src", &["rm", "--cached", "-r", "-q"]),
        ("src/deep", &["rm", "-r"]),
        ("untracked", &["rm", "-r"]),
        ("untracked", &["rm", "-r", "--ignore-unmatch"]),
        (".", &["rm", "-r"]),
    ];
    for (cwd, args) in cases {
        let mut seen = Vec::new();
        for (who, bin) in [("stock", stock), ("zvcs", BIN)] {
            let dir = fixture(who, stock);
            let result = run(bin, &dir.join(cwd), args);
            let state = run(stock, &dir, &["status", "--porcelain"]);
            let _ = std::fs::remove_dir_all(&dir);
            seen.push((result, state));
        }
        assert_eq!(seen[1], seen[0], "cwd {cwd} args {args:?}");
    }
}
