//! `git push` with no `<remote>` resolves the default remote in git's order:
//! `branch.<name>.pushRemote`, `remote.pushDefault`, `branch.<name>.remote`,
//! then `origin`. Regression guard for the remote being hardcoded to `origin`.
//!
//! zvcs has no send-pack, so push fails its pre-flight naming the resolved
//! remote — which is exactly what these tests assert.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git").args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-pushcfg-{tag}-{}", std::process::id()));
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
    std::fs::write(repo.join("f"), "x\n").unwrap();
    git(&repo, &["add", "f"]);
    git(&repo, &["commit", "-q", "-m", "c0"]);
    for r in ["origin", "backup", "other"] {
        git(&repo, &["remote", "add", r, &format!("https://example.com/{r}.git")]);
    }
    (repo, home)
}

/// The remote `push` resolved to, extracted from its pre-flight error `... to <remote> (<url>)`.
fn resolved_remote(repo: &Path, home: &Path) -> String {
    let out = Command::new(BIN)
        .arg("push")
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    // "... cannot upload main to <remote> (<url>)"
    err.rsplit_once(" to ")
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .unwrap_or("")
        .to_string()
}

#[test]
fn push_default_remote_resolution_order() {
    let (repo, home) = fixture("order");

    // No config → origin.
    assert_eq!(resolved_remote(&repo, &home), "origin");

    // remote.pushDefault takes over.
    git(&repo, &["config", "remote.pushDefault", "backup"]);
    assert_eq!(resolved_remote(&repo, &home), "backup");

    // branch.<name>.pushRemote overrides remote.pushDefault.
    git(&repo, &["config", "branch.main.pushRemote", "other"]);
    assert_eq!(resolved_remote(&repo, &home), "other");

    // With neither pushRemote nor pushDefault, fall back to branch.<name>.remote.
    git(&repo, &["config", "--unset", "branch.main.pushRemote"]);
    git(&repo, &["config", "--unset", "remote.pushDefault"]);
    git(&repo, &["config", "branch.main.remote", "backup"]);
    assert_eq!(resolved_remote(&repo, &home), "backup");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
