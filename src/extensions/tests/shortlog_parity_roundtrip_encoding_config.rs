//! `core.checkRoundtripEncoding` is read by git as a plain string (`git_config_string()`) and
//! only compared with a file's encoding when a round trip is checked, so a label no encoding
//! answers to — `all`, say — never matches and is never an error. The path-limited `shortlog`
//! builds a diff resource cache, whose filter pipeline options rejected the value with
//! `Could not obtain resource cache for diffing`.

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
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
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
    let dir = std::env::temp_dir().join(format!(
        "zvcs-roundtrip-enc-{}-{}",
        std::process::id(),
        if bin == BIN { "zvcs" } else { "stock" }
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    run(bin, &dir, &["init", "-q", "-b", "main", "."]);
    std::fs::write(dir.join("a"), "a\n").unwrap();
    run(bin, &dir, &["add", "a"]);
    run(bin, &dir, &["commit", "-q", "-m", "one"]);
    std::fs::write(dir.join("a"), "a\nb\n").unwrap();
    run(bin, &dir, &["commit", "-q", "-a", "-m", "two"]);
    dir
}

#[test]
fn an_unknown_label_is_not_an_error() {
    let Some(stock) = stock_git() else { return };
    let (s, z) = (fixture(stock), fixture(BIN));
    for value in ["all", "bogus,utf-8", "SHIFT-JIS UTF-16", "", "utf-8"] {
        let cfg = format!("core.checkRoundtripEncoding={value}");
        let vectors: &[&[&str]] = &[
            &["shortlog", "HEAD", "--", "a"],
            &["shortlog", "-sn", "HEAD", "--", "a"],
            &["diff-tree", "-r", "-p", "HEAD"],
            &["log", "-p", "HEAD", "--", "a"],
            &["diff", "HEAD~1", "HEAD"],
        ];
        for tail in vectors {
            let mut args = vec!["-c", cfg.as_str()];
            args.extend_from_slice(tail);
            assert_eq!(run(BIN, &z, &args), run(stock, &s, &args), "{args:?}");
        }
    }
    let _ = (std::fs::remove_dir_all(s), std::fs::remove_dir_all(z));
}
