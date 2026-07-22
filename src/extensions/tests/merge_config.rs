//! `git merge` honors `merge.ff` as the fast-forward default, with the CLI
//! (`--ff-only`/`--ff`/`--no-ff`) still overriding. Regression guard for the
//! config being ignored (always fast-forwarding a linear history).

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git").args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

/// A repo where `feat` is strictly ahead of `main` (a fast-forwardable merge),
/// with `main` checked out.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-mergecfg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@e.x"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("f"), "base\n").unwrap();
    git(&repo, &["add", "f"]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    git(&repo, &["checkout", "-q", "-b", "feat"]);
    std::fs::write(repo.join("f"), "base\nmore\n").unwrap();
    git(&repo, &["add", "f"]);
    git(&repo, &["commit", "-q", "-m", "feat"]);
    git(&repo, &["checkout", "-q", "main"]);
    (repo, home)
}

fn run(repo: &Path, home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home)
        .output()
        .unwrap()
}

/// Number of parents of HEAD (2 = a merge commit, 1 = fast-forward tip).
fn head_parents(repo: &Path) -> usize {
    let out = Command::new("git")
        .args(["rev-list", "--parents", "-n", "1", "HEAD"])
        .current_dir(repo)
        .output()
        .unwrap();
    // Line is "<commit> <parent1> [<parent2> ...]" — parents = words - 1.
    String::from_utf8_lossy(&out.stdout).split_whitespace().count() - 1
}

#[test]
fn merge_ff_false_forces_a_merge_commit() {
    let (repo, home) = fixture("noff");
    git(&repo, &["config", "merge.ff", "false"]);
    let out = run(&repo, &home, &["merge", "feat", "-m", "merge feat"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(head_parents(&repo), 2, "merge.ff=false must create a merge commit");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn default_fast_forwards() {
    let (repo, home) = fixture("ff");
    let out = run(&repo, &home, &["merge", "feat"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(head_parents(&repo), 1, "a linear history should fast-forward by default");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn ff_only_flag_overrides_merge_ff_false() {
    let (repo, home) = fixture("ffonly");
    git(&repo, &["config", "merge.ff", "false"]);
    let out = run(&repo, &home, &["merge", "--ff-only", "feat"]);
    assert!(out.status.success(), "--ff-only should still fast-forward: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(head_parents(&repo), 1, "--ff-only must override merge.ff=false");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
