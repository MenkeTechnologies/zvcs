//! git's `advice.*` hints are gated on their config slot. Each hint prints by
//! default and is suppressed by `advice.<slot> = false`, while the non-hint
//! lines around it always print. Regression guard for hints that advertised
//! `advice.<slot>` but never read it.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git").args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-advice-{tag}-{}", std::process::id()));
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
    (repo, home)
}

fn run(repo: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}

/// `git add` with no pathspec: the `addEmptyPathspec` hint shows by default and
/// disappears when the slot is false; the "Nothing specified" line always shows.
#[test]
fn add_empty_pathspec_hint_is_gated() {
    let (repo, home) = fixture("emptypathspec");

    let out = run(&repo, &home, &["add"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("Nothing specified, nothing added."), "err:\n{err}");
    assert!(err.contains("git add ."), "hint should show by default:\n{err}");

    git(&repo, &["config", "advice.addEmptyPathspec", "false"]);
    let out = run(&repo, &home, &["add"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("Nothing specified, nothing added."), "non-hint line must remain:\n{err}");
    assert!(!err.contains("git add ."), "hint must be suppressed:\n{err}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `git branch <invalid>`: the `refSyntax` hint is gated, the fatal line is not.
#[test]
fn branch_ref_syntax_hint_is_gated() {
    let (repo, home) = fixture("refsyntax");

    let out = run(&repo, &home, &["branch", "bad..name"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not a valid branch name"), "err:\n{err}");
    assert!(err.contains("check-ref-format"), "hint should show by default:\n{err}");

    git(&repo, &["config", "advice.refSyntax", "false"]);
    let out = run(&repo, &home, &["branch", "bad..name"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not a valid branch name"), "fatal line must remain:\n{err}");
    assert!(!err.contains("check-ref-format"), "hint must be suppressed:\n{err}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
