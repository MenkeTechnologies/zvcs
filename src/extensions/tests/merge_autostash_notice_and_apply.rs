//! `git merge --autostash`: which exits leave the stash under `MERGE_AUTOSTASH`
//! with the `When finished, apply stashed changes with `git stash pop`` notice,
//! and which apply it.
//!
//! The notice is one `if (autostash) printf(...)` on `cmd_merge()`'s strategy
//! tail (builtin/merge.c:1873-1874), keyed on the option: a conflict, `--squash`
//! and `--no-commit` each print it exactly once, clean worktree or dirty. Every
//! other early exit applies the stash — a refused fast-forward checkout (:1682),
//! a merge no strategy handled (:1846) and `finish()` with a new head (:539-541),
//! which a fast-forward `--squash` reaches without moving `HEAD`.
//!
//! That last case is what the re-apply must survive: `stash apply` merges onto
//! the *index* tree (builtin/stash.c:661-663, 711), so the staged squash result
//! stays. Merging onto `HEAD` instead reverts every squashed path silently.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");
const NOTICE: &str = "When finished, apply stashed changes with `git stash pop`";

fn run(repo: &Path, args: &[&str]) -> Output {
    Command::new(BIN).args(args).current_dir(repo).output().unwrap()
}

fn git(repo: &Path, args: &[&str]) {
    let out = run(repo, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `base` on `main` with `f` and `g`; `side` rewrites `f`. `diverge` gives `main`
/// a commit of its own (touching `f` too when `conflict`), so the merge needs a
/// strategy; without it `side` is a fast-forward.
fn fixture(tag: &str, diverge: bool, conflict: bool) -> PathBuf {
    let repo = std::env::temp_dir().join(format!("zvcs-mautostash-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&repo);
    std::fs::create_dir_all(&repo).unwrap();
    let repo = repo.canonicalize().unwrap();

    git(&repo, &["init", "-q", "-b", "main", "."]);
    git(&repo, &["config", "user.email", "user@example.com"]);
    git(&repo, &["config", "user.name", "User"]);
    std::fs::write(repo.join("f"), "base\n").unwrap();
    std::fs::write(repo.join("g"), "x\n").unwrap();
    git(&repo, &["add", "f", "g"]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    git(&repo, &["checkout", "-q", "-b", "side"]);
    std::fs::write(repo.join("f"), "side\n").unwrap();
    git(&repo, &["commit", "-q", "-a", "-m", "side"]);
    git(&repo, &["checkout", "-q", "main"]);
    if diverge {
        if conflict {
            std::fs::write(repo.join("f"), "main\n").unwrap();
            git(&repo, &["commit", "-q", "-a", "-m", "main"]);
        } else {
            std::fs::write(repo.join("h"), "h\n").unwrap();
            git(&repo, &["add", "h"]);
            git(&repo, &["commit", "-q", "-m", "main"]);
        }
    }
    repo
}

fn notices(out: &Output) -> usize {
    stdout(out).lines().filter(|l| *l == NOTICE).count()
}

#[test]
fn a_conflict_over_a_dirty_worktree_prints_the_notice_once() {
    let repo = fixture("conflict", true, true);
    std::fs::write(repo.join("g"), "dirty\n").unwrap();
    let out = run(&repo, &["merge", "--autostash", "side"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(notices(&out), 1, "stdout: {}", stdout(&out));
    assert!(repo.join(".git/MERGE_AUTOSTASH").exists());
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn squash_and_no_commit_print_the_notice_on_a_clean_worktree_too() {
    for flag in ["--squash", "--no-commit"] {
        let repo = fixture(flag.trim_start_matches('-'), true, false);
        let out = run(&repo, &["merge", "--autostash", flag, "side"]);
        assert_eq!(out.status.code(), Some(0), "{flag}");
        assert_eq!(notices(&out), 1, "{flag} stdout: {}", stdout(&out));
        let _ = std::fs::remove_dir_all(&repo);
    }
}

#[test]
fn a_fast_forward_squash_applies_the_stash_and_keeps_the_squash_staged() {
    let repo = fixture("squashff", false, false);
    std::fs::write(repo.join("g"), "dirty\n").unwrap();
    let out = run(&repo, &["merge", "--autostash", "--squash", "side"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(notices(&out), 0, "stdout: {}", stdout(&out));
    assert!(String::from_utf8_lossy(&out.stderr).contains("Applied autostash.\n"));
    assert!(!repo.join(".git/MERGE_AUTOSTASH").exists());

    assert_eq!(std::fs::read_to_string(repo.join("f")).unwrap(), "side\n", "squashed path reverted");
    assert_eq!(std::fs::read_to_string(repo.join("g")).unwrap(), "dirty\n");
    assert_eq!(stdout(&run(&repo, &["status", "--short"])), "M  f\n M g\n");
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn a_refused_fast_forward_checkout_applies_the_stash() {
    let repo = fixture("ffrefused", false, false);
    git(&repo, &["checkout", "-q", "side"]);
    std::fs::write(repo.join("h"), "h\n").unwrap();
    git(&repo, &["add", "h"]);
    git(&repo, &["commit", "-q", "-m", "h"]);
    git(&repo, &["checkout", "-q", "main"]);
    std::fs::write(repo.join("h"), "untracked\n").unwrap();
    std::fs::write(repo.join("g"), "dirty\n").unwrap();

    let out = run(&repo, &["merge", "--autostash", "side"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(notices(&out), 0, "stdout: {}", stdout(&out));
    assert!(String::from_utf8_lossy(&out.stderr).ends_with("Aborting\nApplied autostash.\n"));
    assert!(!repo.join(".git/MERGE_AUTOSTASH").exists());
    assert_eq!(std::fs::read_to_string(repo.join("g")).unwrap(), "dirty\n");
    let _ = std::fs::remove_dir_all(&repo);
}
