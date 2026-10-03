//! The directory `git clone <repo>` creates, and the sources it refuses before
//! guessing one.
//!
//! `git_url_basename()` (dir.c) strips trailing slashes and then one `/.git`, so
//! `src/.git` and `src/.git/` both clone into `src`; it drops `.bundle` rather
//! than `.git` when `get_repo_path()` found a bundle, so `b.bundle` clones into
//! `b` (`b.git` when bare). `cmd_clone()` refuses a source that
//! `get_repo_path()` cannot read as a repository, gitfile or bundle and that
//! has no colon to be a URL — an existing directory without a repository too:
//! `repository '<repo>' does not exist`. Expectations captured from stock git
//! 2.56.0.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "zvcs-clone-guessed-dir-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn git(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .current_dir(dir)
        .output()
        .expect("run the binary under test")
}

/// `root/src` with one commit, and `root/b.bundle` made from it.
fn fixture(name: &str) -> PathBuf {
    let root = scratch(name);
    git(&root, &["init", "-q", "-b", "main", "src"]);
    git(&root.join("src"), &["commit", "-q", "--allow-empty", "-m", "c1"]);
    git(&root.join("src"), &["bundle", "create", "-q", "../b.bundle", "--all"]);
    root
}

fn clones_into(root: &Path, args: &[&str], dir: &str) {
    let out = git(root, args);
    assert_eq!(out.status.code(), Some(0), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    assert!(root.join(dir).is_dir(), "{args:?} did not create {dir}");
}

#[test]
fn a_trailing_dot_git_component_is_not_the_name() {
    let root = fixture("dotgit");
    let work = root.join("w");
    std::fs::create_dir_all(&work).unwrap();
    clones_into(&work, &["clone", "-q", "../src/.git"], "src");
    std::fs::remove_dir_all(work.join("src")).unwrap();
    clones_into(&work, &["clone", "-q", "../src/.git/"], "src");
}

#[test]
fn a_bundle_drops_its_bundle_suffix() {
    let root = fixture("bundle");
    clones_into(&root, &["clone", "-q", "b.bundle"], "b");
    clones_into(&root, &["clone", "-q", "--bare", "b.bundle"], "b.git");
}

#[test]
fn a_directory_without_a_repository_does_not_exist() {
    let root = fixture("refuse");
    std::fs::create_dir_all(root.join("empty")).unwrap();
    for source in ["empty", "/"] {
        let out = git(&root, &["clone", source]);
        assert_eq!(out.status.code(), Some(128), "{source}");
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            format!("fatal: repository '{source}' does not exist\n"),
            "{source}"
        );
    }
}
