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

    let out = zvcs(&home, &root, &["zreindex", "--sync", root.to_str().unwrap()]);
    assert!(
        out.contains("indexed 2 repo(s)"),
        "expected 2 repos indexed, got: {out}"
    );

    let listed = zvcs(&home, &root, &["zrepos"]);
    assert!(listed.contains("alpha"), "zrepos missing alpha:\n{listed}");
    assert!(listed.contains("beta"), "zrepos missing beta:\n{listed}");
    // Pipe-clean: no count/hint on stdout (piped, non-tty).
    assert!(
        !listed.contains("repo(s)"),
        "zrepos stdout must be pipe-clean (no count line):\n{listed}"
    );

    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn zreindex_prunes_deleted_repos() {
    let home = tmp("prune-home");
    let root = tmp("prune-root");
    for name in ["keep", "gone"] {
        let r = root.join(name);
        std::fs::create_dir_all(&r).unwrap();
        git(&r, &["init", "-q", "-b", "main"]);
    }

    zvcs(&home, &root, &["zreindex", "--sync", root.to_str().unwrap()]);
    let before = zvcs(&home, &root, &["zrepos"]);
    assert!(before.contains("keep") && before.contains("gone"), "both indexed:\n{before}");

    // Delete one repo from disk, then reindex → it must be pruned.
    std::fs::remove_dir_all(root.join("gone")).unwrap();
    let out = zvcs(&home, &root, &["zreindex", "--sync", root.to_str().unwrap()]);
    assert!(out.contains("pruned 1"), "expected 1 pruned, got: {out}");

    let after = zvcs(&home, &root, &["zrepos"]);
    assert!(after.contains("keep"), "kept repo must remain:\n{after}");
    assert!(!after.contains("gone"), "deleted repo must be pruned:\n{after}");

    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn zjobs_and_zrepos_are_empty_without_a_ledger() {
    let home = tmp("empty-home");
    let cwd = tmp("empty-cwd");

    // No db exists under this fresh ZVCS_HOME → graceful empties, not errors.
    assert!(zvcs(&home, &cwd, &["zjobs"]).contains("no jobs"));
    // zrepos is pipe-clean: empty stdout when there's nothing (hint → stderr/tty).
    assert!(
        zvcs(&home, &cwd, &["zrepos"]).trim().is_empty(),
        "empty zrepos must produce clean (empty) stdout"
    );

    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&cwd);
}
