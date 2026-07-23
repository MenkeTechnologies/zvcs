//! Command dispatch beyond builtins, so zvcs can fully shadow `git`:
//!   * external `git-<cmd>` execution — git.c's `execv_dashed_external`, the
//!     mechanism third-party subcommands (`git fuzzy`, `git lfs`, …) rely on;
//!   * dashed argv[0] invocation — `git-status` dispatches `status`;
//!   * builtin precedence over a same-named external;
//!   * no re-exec loop when invoked as a dashed *non*-verb;
//!   * `git zdashed` installs functional `git-<verb>` links.

use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn tmp(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("zvcs-extd-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(&p).unwrap();
    p
}

/// Prepend `dir` to the inherited PATH so a real `/bin/sh` stays reachable for the
/// script shebang while our fake `git-*` is found first.
fn path_with(dir: &Path) -> String {
    format!("{}:{}", dir.display(), std::env::var("PATH").unwrap_or_default())
}

/// Write an executable `git-<name>` shell script into `dir`.
fn write_external(dir: &Path, name: &str, body: &str) {
    let p = dir.join(name);
    fs::write(&p, body).unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn external_git_dash_command_is_executed() {
    let dir = tmp("ext");
    write_external(&dir, "git-greet", "#!/bin/sh\necho \"GREET:$*\"\n");
    let out = Command::new(BIN)
        .args(["greet", "alpha", "beta"])
        .current_dir(&dir)
        .env("PATH", path_with(&dir))
        .env("ZVCS_HOME", tmp("ext-home"))
        .output()
        .unwrap();
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        s.contains("GREET:alpha beta"),
        "external git-greet not exec'd.\nstdout: {s}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn builtin_wins_over_same_named_external() {
    let dir = tmp("prec");
    write_external(&dir, "git-status", "#!/bin/sh\necho SHOULD-NOT-RUN\n");
    let repo = tmp("prec-repo");
    Command::new(BIN)
        .args(["init", "-q", "-b", "main"])
        .current_dir(&repo)
        .status()
        .unwrap();
    let out = Command::new(BIN)
        .args(["status", "--porcelain"])
        .current_dir(&repo)
        .env("PATH", path_with(&dir))
        .env("ZVCS_HOME", tmp("prec-home"))
        .output()
        .unwrap();
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        !s.contains("SHOULD-NOT-RUN"),
        "a git-status external shadowed the builtin: {s}"
    );
}

#[test]
fn dashed_argv0_dispatches_the_verb() {
    let dir = tmp("argv0");
    let link = dir.join("git-symbolic-ref");
    symlink(BIN, &link).unwrap();
    let repo = tmp("argv0-repo");
    Command::new(BIN)
        .args(["init", "-q", "-b", "main"])
        .current_dir(&repo)
        .status()
        .unwrap();
    // `symbolic-ref --short HEAD` resolves on an unborn branch (no commit needed).
    let out = Command::new(&link)
        .args(["--short", "HEAD"])
        .current_dir(&repo)
        .env("ZVCS_HOME", tmp("argv0-home"))
        .output()
        .unwrap();
    let s = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        s.trim(),
        "main",
        "invoking as git-symbolic-ref didn't dispatch symbolic-ref.\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn dashed_unknown_verb_errors_without_looping() {
    // Invoked as `git-bogusverb` (a symlink to this binary) with itself on PATH:
    // 'bogusverb' is not a builtin, and the from-dashed guard must stop it from
    // re-execing git-bogusverb forever. A loop would hang output() until the test
    // harness kills it — a plain failure signal.
    let dir = tmp("loop");
    let link = dir.join("git-bogusverb");
    symlink(BIN, &link).unwrap();
    let out = Command::new(&link)
        .current_dir(&dir)
        .env("PATH", path_with(&dir))
        .env("ZVCS_HOME", tmp("loop-home"))
        .output()
        .unwrap();
    let s = String::from_utf8_lossy(&out.stderr);
    assert!(
        s.contains("is not a git command"),
        "expected the unknown-command diagnostic, got stderr: {s}"
    );
}

#[test]
fn zdashed_installs_functional_links() {
    let dir = tmp("zdashed");
    symlink(BIN, dir.join("git")).unwrap(); // links become relative to this `git`
    let out = Command::new(BIN)
        .args(["zdashed", dir.to_str().unwrap()])
        .env("ZVCS_HOME", tmp("zdashed-home"))
        .output()
        .unwrap();
    assert!(out.status.success(), "zdashed failed: {}", String::from_utf8_lossy(&out.stderr));

    let commit_link = dir.join("git-commit");
    assert!(commit_link.exists(), "git-commit link was not installed");
    assert_eq!(
        fs::read_link(&commit_link).unwrap(),
        PathBuf::from("git"),
        "git-commit should be a relative symlink to the sibling git"
    );

    // Functional end to end: an installed link dispatches its verb. Use
    // `git-symbolic-ref --short HEAD`, which resolves on a fresh repo (no commit).
    let repo = tmp("zdashed-repo");
    Command::new(BIN)
        .args(["init", "-q", "-b", "main"])
        .current_dir(&repo)
        .status()
        .unwrap();
    let out = Command::new(dir.join("git-symbolic-ref"))
        .args(["--short", "HEAD"])
        .current_dir(&repo)
        .env("ZVCS_HOME", tmp("zdashed-run-home"))
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "main",
        "installed git-symbolic-ref link didn't dispatch symbolic-ref.\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
