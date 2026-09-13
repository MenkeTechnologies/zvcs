//! `git merge --continue` is `cmd_commit()` with a one-word argv
//! (builtin/merge.c:1456-1470), not a second implementation of the merge commit.
//!
//! What only the real commit path does, and what a private copy silently skips
//! while still writing the right merge commit and exiting 0:
//!
//! - `repo_rerere()` (builtin/commit.c:1964) turns the staged resolution into the
//!   conflict's `postimage` and reports `Recorded resolution for '<path>'.`;
//! - `MERGE_RR` survives `sequencer_post_commit_cleanup()` for rerere to read;
//! - the hooks are commit's: `pre-commit`, `prepare-commit-msg`, `commit-msg`,
//!   `post-commit` — and never `post-merge`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(repo: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(repo)
        .env("GIT_EDITOR", "true")
        .output()
        .unwrap()
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

/// A merge stopped on a one-hunk conflict in `f`, with every commit hook and
/// `post-merge` installed to announce itself on stderr.
fn stopped_merge(tag: &str, rerere: bool) -> PathBuf {
    let repo = std::env::temp_dir().join(format!("zvcs-mcont-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&repo);
    std::fs::create_dir_all(&repo).unwrap();
    let repo = repo.canonicalize().unwrap();

    git(&repo, &["init", "-q", "-b", "main", "."]);
    git(&repo, &["config", "user.email", "user@example.com"]);
    git(&repo, &["config", "user.name", "User"]);
    git(&repo, &["config", "rerere.enabled", if rerere { "true" } else { "false" }]);

    std::fs::write(repo.join("f"), "a\nb\nc\n").unwrap();
    git(&repo, &["add", "f"]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    git(&repo, &["checkout", "-q", "-b", "side"]);
    std::fs::write(repo.join("f"), "a\nSIDE\nc\n").unwrap();
    git(&repo, &["commit", "-q", "-a", "-m", "side"]);
    git(&repo, &["checkout", "-q", "main"]);
    std::fs::write(repo.join("f"), "a\nMAIN\nc\n").unwrap();
    git(&repo, &["commit", "-q", "-a", "-m", "main"]);

    let out = run(&repo, &["merge", "side"]);
    assert_eq!(out.status.code(), Some(1), "the merge must stop on the conflict");

    for hook in ["pre-commit", "prepare-commit-msg", "commit-msg", "post-commit", "post-merge"] {
        let path = repo.join(".git/hooks").join(hook);
        std::fs::write(&path, format!("#!/bin/sh\necho hook {hook} >&2\n")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::fs::write(repo.join("f"), "a\nRESOLVED\nc\n").unwrap();
    git(&repo, &["add", "f"]);
    repo
}

#[test]
fn continue_records_the_resolution_and_runs_commits_hooks() {
    let repo = stopped_merge("rr", true);
    let out = run(&repo, &["merge", "--continue"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "hook pre-commit\nhook prepare-commit-msg\nhook commit-msg\n\
         Recorded resolution for 'f'.\nhook post-commit\n"
    );

    let postimages: Vec<_> = std::fs::read_dir(repo.join(".git/rr-cache"))
        .unwrap()
        .flatten()
        .filter(|e| e.path().join("postimage").is_file())
        .collect();
    assert_eq!(postimages.len(), 1, "the resolution must be recorded as a postimage");
    assert!(repo.join(".git/MERGE_RR").exists(), "MERGE_RR is left for rerere, not unlinked");
    assert!(!repo.join(".git/MERGE_HEAD").exists());
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn continue_without_rerere_still_runs_commits_hooks_only() {
    let repo = stopped_merge("norr", false);
    let out = run(&repo, &["merge", "--continue"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "hook pre-commit\nhook prepare-commit-msg\nhook commit-msg\nhook post-commit\n"
    );
    assert!(!repo.join(".git/rr-cache").exists());
    let _ = std::fs::remove_dir_all(&repo);
}
