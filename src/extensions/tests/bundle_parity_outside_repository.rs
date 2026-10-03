//! `git bundle verify` and `git bundle create` outside a repository.
//!
//! `bundle` is `RUN_SETUP_GENTLY`; each subcommand asks
//! `startup_info->have_repository` itself (builtin/bundle.c):
//!
//! * `verify` asks before `open_bundle()`, and answers with `error()` — so even
//!   a bundle file that does not exist gets `need a repository to verify a
//!   bundle`, exit 1;
//! * `create` dies: `Need a repository to create a bundle.`, exit 128;
//! * `list-heads` needs no repository at all.
//!
//! Expectations captured from stock git 2.56.0.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "zvcs-bundle-outside-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn git(root: &Path, dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .env("HOME", root)
        .env("GIT_CEILING_DIRECTORIES", root)
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

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// `root/up` holds one commit; `root/out` is no repository and holds
/// `b.bundle` made from `up`.
fn fixture(name: &str) -> (PathBuf, PathBuf) {
    let root = scratch(name);
    let up = root.join("up");
    git(&root, &root, &["init", "-q", "-b", "main", "up"]);
    git(&root, &up, &["commit", "-q", "--allow-empty", "-m", "c1"]);
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    git(&root, &up, &["bundle", "create", "-q", "../out/b.bundle", "--all"]);
    (root, out)
}

#[test]
fn verify_wants_a_repository_before_it_opens_the_file() {
    let (root, out) = fixture("verify");
    for args in [
        &["bundle", "verify", "b.bundle"][..],
        &["bundle", "verify", "-q", "b.bundle"][..],
        &["bundle", "verify", "nope"][..],
    ] {
        let o = git(&root, &out, args);
        assert_eq!(o.status.code(), Some(1), "{args:?}");
        assert_eq!(stderr(&o), "error: need a repository to verify a bundle\n", "{args:?}");
        assert!(o.stdout.is_empty(), "{args:?}");
    }
}

#[test]
fn create_dies_without_a_repository() {
    let (root, out) = fixture("create");
    let o = git(&root, &out, &["bundle", "create", "x.bundle", "HEAD"]);
    assert_eq!(o.status.code(), Some(128));
    assert_eq!(stderr(&o), "fatal: Need a repository to create a bundle.\n");
    assert!(!out.join("x.bundle").exists());
}

#[test]
fn list_heads_needs_none() {
    let (root, out) = fixture("list-heads");
    let o = git(&root, &out, &["bundle", "list-heads", "b.bundle"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(String::from_utf8_lossy(&o.stdout).contains(" refs/heads/main\n"));
}
