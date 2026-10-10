//! `setup_revisions()` stops parsing options at the first word that is not a revision and
//! runs `verify_filename()` over every remaining word, so a dash-word after the first path is
//! `fatal: option '<w>' must come before non-option arguments` (exit 128), and a missing path
//! is the short `no such path` form. `diff-index` parsed the late option (or printed its usage
//! block) instead.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "a")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "a")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("LC_ALL", "C")
        .output()
        .expect("run git");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

fn fixture(bin: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join(format!("zvcs-dixafter-{}-{}", std::process::id(), if bin == BIN { "zvcs" } else { "stock" }));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    run(bin, &dir, &["init", "-q", "-b", "main", "."]);
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    std::fs::write(dir.join("src/b.rs"), "b\n").unwrap();
    run(bin, &dir, &["add", "."]);
    run(bin, &dir, &["commit", "-q", "-m", "one"]);
    std::fs::write(dir.join("a.txt"), "changed\n").unwrap();
    dir
}

#[test]
fn a_dash_word_after_the_first_path_is_never_an_option() {
    let Some(stock) = stock_git() else { return };
    let (s, z) = (fixture(stock), fixture(BIN));
    let vectors: &[&[&str]] = &[
        &["diff-index", "HEAD", "a.txt", "--submodule=-1", "-U0"],
        &["diff-index", "a.txt", "--summary", "--patch-with-raw"],
        &["diff-index", "HEAD", "a.txt", "-m"],
        &["diff-index", "HEAD", "a.txt", "nosuch", "-m"],
        &["diff-index", "HEAD", "a.txt", "-m", "nosuch"],
        &["diff-index", "HEAD", "a.txt", "nosuch"],
        &["diff-index", "HEAD", "a.txt", "src/*.rs"],
        &["diff-index", "HEAD", "a.txt", "src"],
        // an option before the first path is still an option
        &["diff-index", "--summary", "HEAD", "a.txt"],
    ];
    for args in vectors {
        assert_eq!(run(BIN, &z, args), run(stock, &s, args), "{args:?}");
    }
    let _ = (std::fs::remove_dir_all(s), std::fs::remove_dir_all(z));
}
