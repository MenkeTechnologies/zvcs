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

/// A repo where `feat` is strictly ahead of the integration branch `branch` (a
/// fast-forwardable merge), with `branch` checked out.
fn fixture_on(tag: &str, branch: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-mergecfg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", branch]);
    git(&repo, &["config", "user.email", "t@e.x"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("f"), "base\n").unwrap();
    git(&repo, &["add", "f"]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    git(&repo, &["checkout", "-q", "-b", "feat"]);
    std::fs::write(repo.join("f"), "base\nmore\n").unwrap();
    git(&repo, &["add", "f"]);
    git(&repo, &["commit", "-q", "-m", "feat"]);
    git(&repo, &["checkout", "-q", branch]);
    (repo, home)
}

/// A repo whose integration branch is `main`.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    fixture_on(tag, "main")
}

/// Subject line (`%s`) of `HEAD`'s commit.
fn subject(repo: &Path) -> String {
    let out = Command::new("git")
        .args(["log", "-1", "--format=%s"])
        .current_dir(repo)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
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

// `merge.suppressDest`: the default merge message's ` into <branch>` title
// suffix is dropped when the current branch matches one of the (multi-valued,
// glob) patterns. Unset, the list defaults to `main`/`master`.

#[test]
fn default_suppresses_into_main() {
    let (repo, home) = fixture_on("sd-defmain", "main");
    let out = run(&repo, &home, &["merge", "--no-ff", "feat"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(subject(&repo), "Merge branch 'feat'", "main is suppressed by default");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn unmatched_branch_keeps_into_suffix() {
    let (repo, home) = fixture_on("sd-dev", "dev");
    let out = run(&repo, &home, &["merge", "--no-ff", "feat"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(subject(&repo), "Merge branch 'feat' into dev", "dev is not a default-suppressed branch");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn suppress_dest_matches_current_branch() {
    let (repo, home) = fixture_on("sd-match", "dev");
    git(&repo, &["config", "merge.suppressDest", "dev"]);
    let out = run(&repo, &home, &["merge", "--no-ff", "feat"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(subject(&repo), "Merge branch 'feat'", "merge.suppressDest=dev must suppress ' into dev'");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn suppress_dest_replaces_builtin_default() {
    // Setting the variable replaces the built-in main/master default, so main
    // is no longer suppressed.
    let (repo, home) = fixture_on("sd-repl", "main");
    git(&repo, &["config", "merge.suppressDest", "dev"]);
    let out = run(&repo, &home, &["merge", "--no-ff", "feat"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(subject(&repo), "Merge branch 'feat' into main", "an explicit list drops the main/master default");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn suppress_dest_glob_matches() {
    let (repo, home) = fixture_on("sd-glob", "release");
    git(&repo, &["config", "merge.suppressDest", "re*"]);
    let out = run(&repo, &home, &["merge", "--no-ff", "feat"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(subject(&repo), "Merge branch 'feat'", "the glob re* must match release");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn suppress_dest_is_case_sensitive() {
    let (repo, home) = fixture_on("sd-case", "release");
    git(&repo, &["config", "merge.suppressDest", "RE*"]);
    let out = run(&repo, &home, &["merge", "--no-ff", "feat"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(subject(&repo), "Merge branch 'feat' into release", "wildmatch here is case-sensitive");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn suppress_dest_empty_value_clears_the_list() {
    // An empty value wipes the accumulated patterns (including the default);
    // the trailing `xyz` does not match main, so the suffix survives.
    let (repo, home) = fixture_on("sd-clear", "main");
    git(&repo, &["config", "--add", "merge.suppressDest", ""]);
    git(&repo, &["config", "--add", "merge.suppressDest", "xyz"]);
    let out = run(&repo, &home, &["merge", "--no-ff", "feat"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(subject(&repo), "Merge branch 'feat' into main", "empty value clears prior patterns");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
