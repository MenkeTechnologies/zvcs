//! `git update-ref` reflog writing is gated by `core.logAllRefUpdates`, which the
//! gitoxide ref store consults transparently (`repo.refs.write_reflog`) when the
//! repository is opened. update-ref stages edits with `RefLog::AndReference`, so
//! the store's policy decides whether `.git/logs/<ref>` is appended.
//!
//! git's semantics, mirrored here:
//!   * default in a repo with a working tree: log HEAD and refs under
//!     refs/heads, refs/remotes, refs/notes (default-true set),
//!   * `core.logAllRefUpdates=false`: never log,
//!   * `core.logAllRefUpdates=always`: log every ref, even outside the set.
//!
//! These are regression guards: if a future gix bump or an update-ref change
//! stopped honoring the config, the reflog would appear (false case) or vanish
//! (default/always cases) and these tests would catch it. Reference behavior was
//! confirmed byte-for-byte against stock git 2.55.0.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git").args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

/// A repo with one commit, plus an isolated HOME so no user/global config leaks in.
fn fixture(tag: &str) -> (PathBuf, PathBuf, String) {
    let root = std::env::temp_dir().join(format!("zvcs-urefcfg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "alice@example.com"]);
    git(&repo, &["config", "user.name", "Alice"]);
    std::fs::write(repo.join("f"), "hello\n").unwrap();
    git(&repo, &["add", "f"]);
    git(&repo, &["commit", "-q", "-m", "c0"]);
    let sha = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&repo)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    (repo, home, sha)
}

fn update_ref(repo: &Path, home: &Path, args: &[&str]) -> Output {
    let mut a = vec!["update-ref"];
    a.extend_from_slice(args);
    Command::new(BIN)
        .args(&a)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home)
        .output()
        .unwrap()
}

fn reflog_path(repo: &Path, refname: &str) -> PathBuf {
    repo.join(".git").join("logs").join(refname)
}

/// Default in a working-tree repo: refs/heads/* are in git's default-true set, so
/// the update appends a reflog line.
#[test]
fn default_logs_branch_ref() {
    let (repo, home, sha) = fixture("default");

    let out = update_ref(&repo, &home, &["refs/heads/x", &sha]);
    assert!(out.status.success(), "update-ref failed: {out:?}");

    let log = reflog_path(&repo, "refs/heads/x");
    let body = std::fs::read_to_string(&log)
        .unwrap_or_else(|e| panic!("expected reflog at {}: {e}", log.display()));
    // `0{40} <new> committer ...` — a single creation line ending in a newline.
    let line = body.lines().next().unwrap();
    assert!(
        line.starts_with(&format!("{} {} ", "0".repeat(40), sha)),
        "reflog line should record the zero->new transition: {line:?}"
    );
    assert!(body.ends_with('\n'), "reflog line must be newline-terminated");
}

/// `core.logAllRefUpdates=false` disables reflogging entirely: no `.git/logs`
/// entry is created for a branch that would otherwise be logged.
#[test]
fn false_suppresses_reflog() {
    let (repo, home, sha) = fixture("false");
    git(&repo, &["config", "core.logAllRefUpdates", "false"]);

    let out = update_ref(&repo, &home, &["refs/heads/x", &sha]);
    assert!(out.status.success(), "update-ref failed: {out:?}");

    let log = reflog_path(&repo, "refs/heads/x");
    assert!(
        !log.exists(),
        "core.logAllRefUpdates=false must write no reflog, found {}",
        log.display()
    );
    // The ref itself must still be created.
    assert!(repo.join(".git/refs/heads/x").exists(), "ref must still be updated");
}

/// `core.logAllRefUpdates=always` logs refs outside the default-true set (here a
/// bespoke `refs/foo/bar` hierarchy that `false`/default would not log).
#[test]
fn always_logs_nonstandard_ref() {
    let (repo, home, sha) = fixture("always");

    // Baseline: without `always`, refs/foo/bar is outside the default set — no log.
    let out = update_ref(&repo, &home, &["refs/foo/bar", &sha]);
    assert!(out.status.success(), "update-ref failed: {out:?}");
    assert!(
        !reflog_path(&repo, "refs/foo/bar").exists(),
        "refs/foo/bar is outside the default-true set and must not be logged by default"
    );

    // With `always`, the same class of ref is logged.
    git(&repo, &["config", "core.logAllRefUpdates", "always"]);
    let out = update_ref(&repo, &home, &["refs/foo/baz", &sha]);
    assert!(out.status.success(), "update-ref failed: {out:?}");
    let log = reflog_path(&repo, "refs/foo/baz");
    assert!(
        log.exists(),
        "core.logAllRefUpdates=always must log refs/foo/baz, missing {}",
        log.display()
    );
}
