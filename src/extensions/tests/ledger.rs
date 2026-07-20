//! Tests for the repo index + job ledger read/control verbs, driven through the
//! real `git` binary with an isolated `ZVCS_HOME` per invocation (no shared
//! `~/.zvcs`, no process-global env, so tests stay parallel-safe).

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(["-c", "user.email=t@example.com", "-c", "user.name=zvcs-test"])
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?} failed");
}

/// Run the zvcs `git` binary with an isolated ZVCS_HOME; return stdout.
fn zvcs(home: &Path, cwd: &Path, args: &[&str]) -> String {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env("ZVCS_HOME", home)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn tmp(tag: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("zvcs-ledger-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p.canonicalize().unwrap()
}

#[test]
fn crawler_indexes_repos_and_zrepos_lists_them() {
    let home = tmp("home");
    let root = tmp("root");

    // Two real repos under `root`, plus a non-repo directory that must be ignored.
    for name in ["alpha", "beta"] {
        let r = root.join(name);
        std::fs::create_dir_all(&r).unwrap();
        git(&r, &["init", "-q", "-b", "main"]);
    }
    std::fs::create_dir_all(root.join("not_a_repo")).unwrap();

    let out = zvcs(&home, &root, &["zreindex", root.to_str().unwrap()]);
    assert!(
        out.contains("indexed 2 repo(s)"),
        "expected 2 repos indexed, got: {out}"
    );

    let listed = zvcs(&home, &root, &["zrepos"]);
    assert!(listed.contains("alpha"), "zrepos missing alpha:\n{listed}");
    assert!(listed.contains("beta"), "zrepos missing beta:\n{listed}");

    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn zjobs_and_zrepos_are_empty_without_a_ledger() {
    let home = tmp("empty-home");
    let cwd = tmp("empty-cwd");

    // No db exists under this fresh ZVCS_HOME → graceful empties, not errors.
    assert!(zvcs(&home, &cwd, &["zjobs"]).contains("no jobs"));
    assert!(zvcs(&home, &cwd, &["zrepos"]).to_lowercase().contains("no repo"));

    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&cwd);
}
