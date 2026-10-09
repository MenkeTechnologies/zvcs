//! `git restore -p` combined with options it refuses.
//!
//! `-p` with `--overlay` is the first combination check of `checkout_main()`
//! (`options '-p' and '--overlay' cannot be used together`, 128), ahead of the
//! `--pathspec-file-nul` requirement and everything else; `-p` with
//! `--pathspec-from-file` is refused before the file is opened. zvcs let `restore --overlay
//! -p` run the hunk selector, and opened the file first.

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
    let dir = std::env::temp_dir().join(format!("zvcs-restorep-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    run(stock, &dir, &["init", "-q", "-b", "main"]);
    run(stock, &dir, &["add", "."]);
    assert_eq!(run(stock, &dir, &["commit", "-qm", "one"]).2, Some(0));
    std::fs::write(dir.join("a.txt"), "edited\n").unwrap();
    dir
}

#[test]
fn patch_mode_conflicts_are_judged_in_checkouts_order() {
    let Some(stock) = stock_git() else { return };
    let cases: [&[&str]; 10] = [
        &["restore", "--overlay", "-p"],
        &["restore", "-p", "--overlay"],
        &["restore", "--overlay", "--patch", "a.txt"],
        &["restore", "--overlay", "--staged", "-p"],
        &["restore", "--overlay", "-p", "--source=HEAD"],
        &["restore", "--overlay", "--pathspec-file-nul", "-p"],
        &["restore", "--overlay", "-p", "--pathspec-from-file=x"],
        &["restore", "-p", "--pathspec-from-file=x"],
        &["restore", "-p", "--pathspec-file-nul"],
        &["restore", "-p", "--no-overlay", "--pathspec-from-file=x"],
    ];
    for args in cases {
        let mut seen = Vec::new();
        for (who, bin) in [("stock", stock), ("zvcs", BIN)] {
            let dir = fixture(who, stock);
            let result = run(bin, &dir, args);
            let _ = std::fs::remove_dir_all(&dir);
            seen.push(result);
        }
        assert_eq!(seen[1], seen[0], "args {args:?}");
    }
}
