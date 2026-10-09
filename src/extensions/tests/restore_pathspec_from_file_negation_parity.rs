//! `--no-pathspec-from-file`, and a lone operand beside `--pathspec-from-file` in `checkout`.
//!
//! Stock accepts `--no-pathspec-from-file` and leaves an earlier `--pathspec-from-file` in
//! force, so a pathspec argument still conflicts with it (add, reset, rm, stash push, commit,
//! restore, checkout). zvcs cleared the option and went on with the arguments.
//!
//! `git checkout --pathspec-from-file=<f> <operand>` takes the operand as the tree-ish only when
//! it names a revision; anything else is a pathspec argument, refused as such. zvcs took it as a
//! tree-ish unconditionally and reported `invalid reference`.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(bin: &str, dir: &Path, args: &[&str]) -> (String, String, Option<i32>) {
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

fn fixture(tag: &str, bin: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-nopff-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    std::fs::write(dir.join("README.md"), "a\n").unwrap();
    git(bin, &dir, &["init", "-q", "-b", "main"]);
    git(bin, &dir, &["add", "."]);
    assert_eq!(git(bin, &dir, &["commit", "-qm", "one"]).2, Some(0));
    dir
}

#[test]
fn no_pathspec_from_file_leaves_an_earlier_pathspec_from_file_in_force() {
    let Some(stock) = stock_git() else { return };
    let mut per_bin = Vec::new();
    for bin in [stock, BIN] {
        let dir = fixture("nopff", bin);
        std::fs::write(dir.join("list"), "README.md\n").unwrap();
        let mut results = Vec::new();
        for args in [
            &["restore", "--pathspec-from-file=list", "--no-pathspec-from-file", "README.md"][..],
            &["restore", "--pathspec-from-file=list", "--no-pathspec-from-file"],
            &["restore", "--no-pathspec-from-file", "README.md"],
            &["checkout", "--pathspec-from-file=list", "--no-pathspec-from-file", "README.md"],
            &["reset", "--pathspec-from-file=list", "--no-pathspec-from-file", "README.md"],
            &["add", "--pathspec-from-file=list", "--no-pathspec-from-file", "README.md"],
            &["checkout", "--pathspec-from-file=list", "README.md"],
            &["checkout", "--pathspec-from-file=list", "no-such-name"],
            &["checkout", "--pathspec-from-file=list", "HEAD", "README.md"],
            &["checkout", "--pathspec-from-file=list", "HEAD"],
            &["rm", "--cached", "--pathspec-from-file=list", "--no-pathspec-from-file", "README.md"],
            &["stash", "push", "--pathspec-from-file=list", "--no-pathspec-from-file", "README.md"],
        ] {
            results.push(git(bin, &dir, args));
        }
        let _ = std::fs::remove_dir_all(&dir);
        per_bin.push(results);
    }
    assert_eq!(per_bin[1], per_bin[0]);
}
