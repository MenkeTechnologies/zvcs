//! `git http-fetch` — what the option scan does with options it has no branch
//! for, with `-h`, and with the `--packfile` / `--index-pack-arg` pairing.
//!
//! http-fetch.c:115-146 (v2.56.0) tests each `-`-prefixed argument by its second
//! character and then by a few exact strings; an argument no branch matches is
//! skipped without a word and only counts towards the argument check at
//! http-fetch.c:147. `-h` calls `usage()` on the spot. `--index-pack-arg=` is
//! collected in the loop and checked against `--packfile` only after the
//! argument count and the repository check (http-fetch.c:157-168).
//!
//! Nothing here opens a URL. Every expectation was measured from stock git
//! 2.56.0 in an empty repository.

use std::path::PathBuf;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

const USAGE: &str = "usage: git http-fetch [-c] [-t] [-a] [-v] [--recover] [-w ref] \
                     [--stdin | --packfile=hash | commit-id] url\n";

struct Repo(PathBuf);

impl Repo {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!("zvcs-httpfetch-unknown-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        let out = Command::new(BIN)
            .args(["init", "-q"])
            .current_dir(&p)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(out.status.success(), "init failed: {out:?}");
        Repo(p)
    }

    /// `(stdout, stderr, exit code)` of `git http-fetch <args>` in this repository.
    fn http_fetch(&self, args: &[&str]) -> (String, String, Option<i32>) {
        let out = Command::new(BIN)
            .arg("http-fetch")
            .args(args)
            .current_dir(&self.0)
            .stdin(Stdio::null())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code(),
        )
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// An unknown option is skipped, so the argument count decides: alone it
/// leaves no URL and stock answers the usage line with 129, not a refusal
/// naming the option.
#[test]
fn an_unknown_option_is_skipped_and_the_argument_count_decides() {
    let repo = Repo::new("skip");
    for args in [&["--no-such-flag"][..], &["--bogus", "a", "b", "c"][..]] {
        assert_eq!(repo.http_fetch(args), (String::new(), USAGE.to_string(), Some(129)), "{args:?}");
    }
}

/// `-h` dies with the usage line at once, even when the argument count is
/// otherwise right and a walk would follow.
#[test]
fn dash_h_prints_the_usage_before_anything_is_fetched() {
    let repo = Repo::new("dashh");
    assert_eq!(
        repo.http_fetch(&["-h", "a", "http://127.0.0.1:1/"]),
        (String::new(), USAGE.to_string(), Some(129))
    );
}

/// The two halves of the pack-download mode name each other when one is
/// missing; the option is spelled `--index-pack-arg` in 2.56.0's messages.
#[test]
fn packfile_and_index_pack_arg_require_each_other() {
    let repo = Repo::new("pairing");
    assert_eq!(
        repo.http_fetch(&["--index-pack-arg=x", "abc", "http://127.0.0.1:1/"]),
        (
            String::new(),
            "fatal: the option '--index-pack-arg' requires '--packfile'\n".to_string(),
            Some(128)
        )
    );
    assert_eq!(
        repo.http_fetch(&[
            "--packfile=0d28038537ef89b27534b41bae77f50b331e6042",
            "http://127.0.0.1:1/"
        ]),
        (
            String::new(),
            "fatal: the option '--packfile' requires '--index-pack-arg'\n".to_string(),
            Some(128)
        )
    );
}

/// `--packfile` drops the commit-id operand from the count, so a pack id plus
/// two operands is one too many.
#[test]
fn packfile_takes_the_url_alone() {
    let repo = Repo::new("pkcount");
    assert_eq!(
        repo.http_fetch(&["--packfile=0d28038537ef89b27534b41bae77f50b331e6042", "x", "y"]),
        (String::new(), USAGE.to_string(), Some(129))
    );
}
