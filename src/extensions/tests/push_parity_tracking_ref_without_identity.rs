//! A push advances the remote-tracking ref even with no identity configured.
//!
//! `update_one_tracking_ref()` (transport.c) logs the update with
//! `git_committer_info(0)` — not `IDENT_STRICT` — so a missing `user.name` /
//! `user.email` falls back to the system identity (`EMAIL`, gecos, host)
//! instead of refusing. The ref and its `update by push` reflog line are
//! written either way. Expectations captured from stock git 2.56.0.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "zvcs-push-tracking-no-ident-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// No `GIT_COMMITTER_*`, no `user.*`: only `EMAIL`, which `ident_default_email()`
/// prefers over the host-derived address, so the result does not depend on the
/// machine's host name.
fn git(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_AUTHOR_NAME")
        .env_remove("GIT_AUTHOR_EMAIL")
        .env_remove("GIT_COMMITTER_NAME")
        .env_remove("GIT_COMMITTER_EMAIL")
        .env("EMAIL", "t@e.x")
        .current_dir(dir)
        .output()
        .expect("run the binary under test")
}

#[test]
fn the_tracking_ref_and_its_reflog_are_written() {
    let root = scratch("push");
    git(&root, &["init", "-q", "--bare", "up.git"]);
    let w = root.join("w");
    std::fs::create_dir_all(&w).unwrap();
    git(&w, &["init", "-q", "-b", "main", "."]);
    git(&w, &["-c", "user.name=t", "-c", "user.email=t@e.x", "commit", "-q", "--allow-empty", "-m", "c1"]);
    git(&w, &["remote", "add", "origin", "../up.git"]);

    let out = git(&w, &["push", "-q", "origin", "main:y"]);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));

    let head = git(&w, &["rev-parse", "HEAD"]);
    let tracking = git(&w, &["rev-parse", "--verify", "-q", "refs/remotes/origin/y"]);
    assert_eq!(tracking.status.code(), Some(0));
    assert_eq!(tracking.stdout, head.stdout);

    let log = std::fs::read_to_string(w.join(".git/logs/refs/remotes/origin/y")).expect("reflog");
    let tip = String::from_utf8_lossy(&head.stdout).trim().to_string();
    assert!(log.starts_with(&format!("{} {tip} ", "0".repeat(40))), "{log}");
    assert!(log.contains(" <t@e.x> "), "{log}");
    assert!(log.ends_with("\tupdate by push\n"), "{log}");
}
