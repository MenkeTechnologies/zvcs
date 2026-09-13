//! The `--no-show-forced-updates` note is `store_updated_refs()`' (builtin/fetch.c:1351-1358):
//! it is printed once a fetch has walked its ref map, so a fetch refused before that point
//! prints only its refusal, and `--negotiate-only`, which never reaches `do_fetch()`, prints
//! nothing but the negotiated ids. Captured from git 2.55.0.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

const NOTE: &str = "warning: fetch normally indicates which branches had a forced update,\n\
but that check has been disabled; to re-enable, use '--show-forced-updates'\n\
flag or run 'git config fetch.showForcedUpdates true'\n";

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("ZVCS_HOME", home)
        .env("LC_ALL", "C")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .output()
        .unwrap()
}

fn ok(dir: &Path, home: &Path, args: &[&str]) {
    let out = run(dir, home, args);
    assert!(
        out.status.success(),
        "setup `git {args:?}` failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// `up` with one commit, `dn` cloned from it, then `up` advanced by one commit.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-fetch-fuadvice-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("home")).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    ok(&root, &home, &["init", "-q", "-b", "main", "up"]);
    ok(&root.join("up"), &home, &["commit", "-q", "--allow-empty", "-m", "c0"]);
    ok(&root, &home, &["clone", "-q", "up", "dn"]);
    ok(&root.join("up"), &home, &["commit", "-q", "--allow-empty", "-m", "c1"]);
    (root.join("dn"), home)
}

#[test]
fn a_fetch_refused_before_its_ref_map_is_walked_prints_no_note() {
    let (dn, home) = fixture("refused");

    let out = run(&dn, &home, &["fetch", "--no-show-forced-updates", "origin", "nosuch"]);
    assert_eq!(stderr(&out), "fatal: couldn't find remote ref nosuch\n");
    assert_eq!(out.status.code(), Some(128));

    let out = run(
        &dn,
        &home,
        &["fetch", "--no-show-forced-updates", "--negotiate-only", "--negotiation-tip=main", "origin"],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stderr(&out), "");
}

#[test]
fn the_note_precedes_the_summary_of_a_fetch_that_updated_refs() {
    let (dn, home) = fixture("updated");
    let out = run(&dn, &home, &["fetch", "--no-show-forced-updates"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.starts_with(NOTE), "{err}");
    assert_eq!(err.matches("warning: fetch normally").count(), 1, "{err}");
    assert!(err[NOTE.len()..].starts_with("From "), "{err}");

    // `-q` silences the summary, not the warning.
    ok(&dn.join("../up"), &home, &["commit", "-q", "--allow-empty", "-m", "c2"]);
    let out = run(&dn, &home, &["fetch", "-q", "--no-show-forced-updates"]);
    assert_eq!(stderr(&out), NOTE);
}
