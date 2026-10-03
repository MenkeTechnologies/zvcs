//! `git remote show` / `prune` / `set-head -a` against a configured URL that is
//! a local path holding no repository.
//!
//! All three reach `transport_get_remote_refs()`, whose `git_connect()` spawns
//! `upload-pack` and dies in `enter_repo()`:
//!
//! ```text
//! fatal: '<path>' does not appear to be a git repository
//! fatal: Could not read from remote repository.
//!
//! Please make sure you have the correct access rights
//! and the repository exists.
//! ```
//!
//! Exit 128. Expectations captured from stock git 2.56.0.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "zvcs-remote-unreachable-path-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn git(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .current_dir(dir)
        .output()
        .expect("run the binary under test")
}

const NOT_A_REPO: &str = "fatal: '../nope' does not appear to be a git repository\n\
     fatal: Could not read from remote repository.\n\n\
     Please make sure you have the correct access rights\n\
     and the repository exists.\n";

#[test]
fn every_querying_subcommand_names_the_path() {
    let w = scratch("w");
    git(&w, &["init", "-q", "-b", "main", "."]);
    git(&w, &["remote", "add", "o", "../nope"]);
    for args in [
        &["remote", "show", "o"][..],
        &["remote", "prune", "o"][..],
        &["remote", "set-head", "o", "-a"][..],
    ] {
        let out = git(&w, args);
        assert_eq!(out.status.code(), Some(128), "{args:?}");
        assert_eq!(String::from_utf8_lossy(&out.stderr), NOT_A_REPO, "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
    }
}
