//! `core.bare` read from `$GIT_DIR/config.worktree` during discovery.
//!
//! `check_repository_format_gently()` (setup.c:787-801, git v2.55.0) re-reads the
//! per-worktree file through `read_worktree_config()` when
//! `extensions.worktreeConfig` is on, and clears `has_common`, so its `core.bare`
//! decides whether discovery installs a work tree — in the main worktree and in a
//! linked one alike. Expectations were taken from stock git 2.55.0 on the same
//! inputs.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env("GIT_AUTHOR_NAME", "a")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "a")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .output()
        .expect("run zvcs git")
}

fn ok(dir: &Path, home: &Path, args: &[&str]) {
    let out = run(dir, home, args);
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// A repository with one commit and `extensions.worktreeConfig` on, beside an
/// empty `$HOME`. Named per test and per pid so concurrent binaries never share.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-wtcfg-bare-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let work = root.join("r");
    std::fs::create_dir_all(&home).expect("mkdir home");
    std::fs::create_dir_all(&work).expect("mkdir work");
    ok(&work, &home, &["init", "-q"]);
    std::fs::write(work.join("a"), "a\n").expect("write a");
    ok(&work, &home, &["add", "a"]);
    ok(&work, &home, &["commit", "-qm", "a"]);
    ok(&work, &home, &["config", "extensions.worktreeConfig", "true"]);
    (root, home)
}

#[test]
fn main_worktree_config_worktree_bare_removes_the_work_tree() {
    let (root, home) = fixture("main");
    let work = root.join("r");
    // `git_config_bool()` spelling, not just `true`.
    std::fs::write(work.join(".git/config.worktree"), "[core]\n\tbare = on\n").expect("write");

    let out = run(&work, &home, &["rev-parse", "--is-bare-repository"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "true\n");

    let out = run(&work, &home, &["status"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(String::from_utf8_lossy(&out.stderr), "fatal: this operation must be run in a work tree\n");
}

#[test]
fn linked_worktree_honours_its_own_config_worktree_bare() {
    let (root, home) = fixture("linked");
    let work = root.join("r");
    ok(&work, &home, &["worktree", "add", "-q", "../lw"]);
    std::fs::write(work.join(".git/worktrees/lw/config.worktree"), "[core]\n\tbare = true\n")
        .expect("write");

    let linked = root.join("lw");
    let out = run(&linked, &home, &["rev-parse", "--is-bare-repository"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "true\n");

    // The main worktree's own (absent) `config.worktree` leaves it non-bare.
    let out = run(&work, &home, &["rev-parse", "--is-bare-repository"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "false\n");
}
