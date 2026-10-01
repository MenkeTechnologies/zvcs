//! git 2.56's `die_if_upstream_looks_like_remote()` (builtin/branch.c:946-967).
//!
//! `git branch -u origin side` — meant as `-u origin/side` — names a branch
//! `side` that does not exist locally. When the upstream has no slash, names a
//! configured remote, and `refs/remotes/<upstream>/<branch>` exists, 2.56
//! replaces the plain "does not exist" with:
//!
//! ```c
//! code = die_message(_("--set-upstream-to takes a single <remote>/<branch> argument"));
//! advise_if_enabled(ADVICE_SET_UPSTREAM_FAILURE,
//!                   _("Did you mean to use: git branch --set-upstream-to=%s/%s?"),
//!                   new_upstream, branch_name);
//! ```
//!
//! `cmd_branch()` (builtin/branch.c:1244-1252) only asks while
//! `advice.setUpstreamFailure` is enabled. Expectations captured from stock
//! git 2.56.0.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn fixture(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "zvcs-branch-upstream-typo-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    git(&dir, &["init", "-q", "-b", "main", "."]);
    git(&dir, &["commit", "-q", "--allow-empty", "-m", "c1"]);
    git(&dir, &["remote", "add", "origin", "."]);
    git(&dir, &["update-ref", "refs/remotes/origin/side", "HEAD"]);
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

#[test]
fn a_remote_and_branch_given_as_two_words_get_the_joined_spelling() {
    let dir = fixture("typo");
    for args in [
        &["branch", "-u", "origin", "side"][..],
        &["branch", "--set-upstream-to=origin", "side"][..],
    ] {
        let out = git(&dir, args);
        assert_eq!(out.status.code(), Some(128), "{args:?}");
        assert_eq!(
            stderr(&out),
            "fatal: --set-upstream-to takes a single <remote>/<branch> argument\n\
             hint: Did you mean to use: git branch --set-upstream-to=origin/side?\n\
             hint: Disable this message with \"git config set advice.setUpstreamFailure false\"\n",
            "{args:?}"
        );
    }
    // Nothing was configured.
    assert!(git(&dir, &["config", "branch.side.merge"]).stdout.is_empty());
}

/// With the advice off, or without the remote-tracking ref, the 2.55 refusal
/// stands.
#[test]
fn otherwise_the_branch_simply_does_not_exist() {
    let dir = fixture("plain");
    let off = git(&dir, &["-c", "advice.setUpstreamFailure=false", "branch", "-u", "origin", "side"]);
    assert_eq!(off.status.code(), Some(128));
    assert_eq!(stderr(&off), "fatal: branch 'side' does not exist\n");

    let missing = git(&dir, &["branch", "-u", "origin", "nosuch"]);
    assert_eq!(missing.status.code(), Some(128));
    assert_eq!(stderr(&missing), "fatal: branch 'nosuch' does not exist\n");

    // An upstream that is not a remote name is not second-guessed.
    let not_remote = git(&dir, &["branch", "-u", "main", "side"]);
    assert_eq!(not_remote.status.code(), Some(128));
    assert_eq!(stderr(&not_remote), "fatal: branch 'side' does not exist\n");
}
