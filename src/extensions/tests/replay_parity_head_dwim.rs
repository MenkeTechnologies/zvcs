//! `git replay` names the branch a `HEAD` (or other symref) revision resolves to.
//!
//! `get_ref_information()` (replay.c) records `repo_dwim_ref()`'s `fullname`,
//! which `expand_ref()` takes from `refs_resolve_ref_unsafe()` (refs.c:821-826):
//! the name at the *end* of the symref chain. So `main..HEAD` with `HEAD` on
//! `side` puts `refs/heads/side` in `update_refs`, and the decoration on the
//! tip moves it; a detached `HEAD` stays `HEAD`. Measured against stock git
//! 2.56.0 on this exact fixture.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@x")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .unwrap()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = run(dir, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `main` is a-e; `side` is a-c.
fn fixture(tag: &str) -> PathBuf {
    let repo = std::env::temp_dir().join(format!("zvcs-replay-head-dwim-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&repo);
    std::fs::create_dir_all(&repo).unwrap();
    let repo = repo.canonicalize().unwrap();
    git(&repo, &["init", "-q", "-b", "main", "."]);
    for (f, branch_after) in [("a", true), ("e", false)] {
        std::fs::write(repo.join(f), format!("{f}\n")).unwrap();
        git(&repo, &["add", f]);
        git(&repo, &["commit", "-q", "-m", f]);
        if branch_after {
            git(&repo, &["branch", "side"]);
        }
    }
    git(&repo, &["checkout", "-q", "side"]);
    std::fs::write(repo.join("c"), "c\n").unwrap();
    git(&repo, &["add", "c"]);
    git(&repo, &["commit", "-q", "-m", "c"]);
    repo
}

const SIDE_OLD: &str = "4d4ca438f47d4600a449a3a37a0db65babe1dfc9";
const SIDE_NEW: &str = "73db3d0a05113d2dea668d69d101f2d47b5dd6f9";

#[test]
fn head_on_a_branch_updates_that_branch() {
    let repo = fixture("attached");
    assert_eq!(git(&repo, &["rev-parse", "side"]), format!("{SIDE_OLD}\n"));
    assert_eq!(
        git(&repo, &["replay", "--ref-action=print", "--onto", "main", "main..HEAD"]),
        format!("update refs/heads/side {SIDE_NEW} {SIDE_OLD}\n")
    );
}

#[test]
fn a_symbolic_branch_resolves_to_its_target() {
    let repo = fixture("alias");
    git(&repo, &["symbolic-ref", "refs/heads/alias", "refs/heads/side"]);
    assert_eq!(
        git(&repo, &["replay", "--ref-action=print", "--onto", "main", "main..alias"]),
        format!("update refs/heads/side {SIDE_NEW} {SIDE_OLD}\n")
    );
}

#[test]
fn detached_head_updates_head() {
    let repo = fixture("detached");
    git(&repo, &["checkout", "-q", "--detach", "side"]);
    assert_eq!(
        git(&repo, &["replay", "--ref-action=print", "--onto", "main", "main..HEAD"]),
        format!("update HEAD {SIDE_NEW} {SIDE_OLD}\n")
    );
}
