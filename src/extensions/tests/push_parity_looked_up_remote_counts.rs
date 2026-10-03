//! The remote `die_if_repo_looks_like_ref()` asks about is a remote from then on.
//!
//! ```c
//! strbuf_add(&name, repo, slash - repo);
//! if (!remote_is_configured(remote_get(name.buf), 0)) {
//! ```
//!
//! (builtin/push.c, git 2.56.) `remote_get()` reaches `make_remote()`, which
//! appends every name it is asked about to `remote_state->remotes`, configured
//! or not. `remotes_remote_for_branch()` then answers "the sole remote" only
//! while that array holds exactly one entry, so a repository with no remotes
//! that pushes to a missing path with a slash in it (`../nope`) now holds two —
//! `../nope` and `..` — and `setup_default_push_refspecs()` sees a triangular
//! push: `simple` pushes the branch to its own name instead of dying for want
//! of an upstream, and the transport is what fails. A path that exists returns
//! before the lookup, and with `advice.pushRepoLooksLikeRef=false` the lookup
//! never runs; both keep the single anonymous remote and the upstream refusal.
//! Expectations captured from stock git 2.56.0.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "zvcs-push-looked-up-remote-{name}-{}",
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

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A work tree with one commit, no remotes, and a bare sibling `up.git`.
fn fixture(name: &str) -> PathBuf {
    let root = scratch(name);
    git(&root, &["init", "-q", "--bare", "up.git"]);
    let work = root.join("w");
    std::fs::create_dir_all(&work).unwrap();
    git(&work, &["init", "-q", "-b", "main", "."]);
    git(&work, &["commit", "-q", "--allow-empty", "-m", "c1"]);
    work
}

const NOT_A_REPO: &str = "fatal: '../nope' does not appear to be a git repository\n\
     fatal: Could not read from remote repository.\n\n\
     Please make sure you have the correct access rights\n\
     and the repository exists.\n";

fn no_upstream(remote: &str) -> String {
    format!(
        "fatal: The current branch main has no upstream branch.\n\
         To push the current branch and set the remote as upstream, use\n\n    \
         git push --set-upstream {remote} main\n\n\
         To have this happen automatically for branches without a tracking\n\
         upstream, see 'push.autoSetupRemote' in 'git help config'.\n\n"
    )
}

#[test]
fn a_missing_path_makes_the_push_triangular() {
    let w = fixture("missing");
    let out = git(&w, &["push", "../nope"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(stderr(&out), NOT_A_REPO);
}

#[test]
fn an_existing_path_stays_the_sole_remote() {
    let w = fixture("existing");
    let out = git(&w, &["push", "../up.git"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(stderr(&out), no_upstream("../up.git"));
}

#[test]
fn without_the_advice_nothing_else_is_looked_up() {
    let w = fixture("advice-off");
    let out = git(&w, &["-c", "advice.pushRepoLooksLikeRef=false", "push", "../nope"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(stderr(&out), no_upstream("../nope"));
}
