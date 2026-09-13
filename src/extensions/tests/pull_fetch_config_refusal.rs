//! `git pull` runs its fetch as a child (`run_fetch()`, builtin/pull.c), and that child's
//! first act is `repo_config(the_repository, git_fetch_config, &config)`
//! (builtin/fetch.c:2607). A value `git_fetch_config()` refuses therefore ends the pull:
//! the child's `fatal:` is printed, `cmd_pull()` turns the failed `run_fetch()` into exit 1,
//! and nothing is integrated. Captured from git 2.55.0.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

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

fn ok(dir: &Path, home: &Path, args: &[&str]) -> String {
    let out = run(dir, home, args);
    assert!(
        out.status.success(),
        "setup `git {args:?}` failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// `up` with one commit, `dn` cloned from it, then `up` advanced so a pull has work to do.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-pull-fetchcfg-{tag}-{}", std::process::id()));
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
fn a_fetch_key_git_fetch_config_refuses_ends_the_pull_at_one() {
    let (dn, home) = fixture("refuse");
    let before = ok(&dn, &home, &["rev-parse", "HEAD"]);

    let cases: &[(&str, &str)] = &[
        ("fetch.prune=bogus", "fatal: bad boolean config value 'bogus' for 'fetch.prune'\n"),
        ("fetch.recurseSubmodules=bogus", "fatal: bad fetch.recursesubmodules argument: bogus\n"),
        ("fetch.output=bogus", "fatal: invalid value for 'fetch.output': 'bogus'\n"),
        ("fetch.parallel=-1", "fatal: fetch.parallel cannot be negative\n"),
    ];
    for (assignment, want) in cases {
        let out = run(&dn, &home, &["-c", assignment, "pull"]);
        assert_eq!(String::from_utf8_lossy(&out.stderr), *want, "for {assignment}");
        assert_eq!(out.status.code(), Some(1), "for {assignment}");
        assert_eq!(ok(&dn, &home, &["rev-parse", "HEAD"]), before, "for {assignment}");
    }

    // The same refusal from the repository's own config file.
    ok(&dn, &home, &["config", "fetch.prune", "bogus"]);
    let out = run(&dn, &home, &["pull"]);
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "fatal: bad boolean config value 'bogus' for 'fetch.prune'\n"
    );
    assert_eq!(out.status.code(), Some(1));
}
