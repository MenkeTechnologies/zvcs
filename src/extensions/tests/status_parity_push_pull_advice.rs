//! git 2.56's pull hint for a push destination distinct from the upstream.
//!
//! 2.56.0 `format_tracking_info()` (`remote.c:2522-2541`) arms
//! `ENABLE_ADVICE_PULL` for the `@{push}` comparison too, and spells the
//! remote and branch out when the push ref is `refs/remotes/<pushremote>/<b>`:
//!
//! ```c
//! if (is_push) {
//!         flags |= ENABLE_ADVICE_PUSH;
//!         if (!upstream_ref || strcmp(upstream_ref, full_ref)) {
//!                 push_remote_name = pushremote_for_branch(branch, NULL);
//!                 if (push_remote_name &&
//!                     skip_prefix(full_ref, "refs/remotes/", &push_branch_name) &&
//!                     skip_prefix(push_branch_name, push_remote_name, &push_branch_name) &&
//!                     *push_branch_name == '/') {
//!                         push_branch_name++;
//!                         flags |= ENABLE_ADVICE_PULL;
//!                 } else {
//!                         push_remote_name = NULL;
//!                 }
//!         } else {
//!                 flags |= ENABLE_ADVICE_PULL;
//!         }
//! }
//! ```
//!
//! `format_branch_comparison()` (`remote.c:2414-2422`) then prints
//! `(use "git pull <remote> <branch>" …)`. The divergence hint stays gated on
//! `is_upstream`, so a diverged push comparison still prints no hint. Every
//! expectation below was captured from stock git 2.56.0 on the same fixture.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "zvcs-status-push-pull-advice-{name}-{}",
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
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .current_dir(dir)
        .output()
        .expect("run the binary under test")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `main` one commit behind both `origin/main` (its upstream) and the push
/// destination `<fork-tracking>/main`, with `remote.pushDefault=fork`.
/// `fork_tracking` is the hierarchy fork's fetch refspec maps into.
fn fixture(name: &str, fork_tracking: &str) -> PathBuf {
    let dir = scratch(name);
    git(&dir, &["init", "-q", "-b", "main", "."]);
    git(&dir, &["commit", "-q", "--allow-empty", "-m", "c1"]);
    git(&dir, &["commit", "-q", "--allow-empty", "-m", "c2"]);
    let fork_fetch = format!("+refs/heads/*:refs/remotes/{fork_tracking}/*");
    for (key, value) in [
        ("remote.origin.url", "."),
        ("remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"),
        ("remote.fork.url", "."),
        ("remote.fork.fetch", fork_fetch.as_str()),
        ("branch.main.remote", "origin"),
        ("branch.main.merge", "refs/heads/main"),
        ("remote.pushDefault", "fork"),
        ("push.default", "current"),
    ] {
        git(&dir, &["config", key, value]);
    }
    git(&dir, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    git(&dir, &["update-ref", &format!("refs/remotes/{fork_tracking}/main"), "HEAD"]);
    git(&dir, &["reset", "-q", "--hard", "HEAD~"]);
    dir
}

fn status_both(dir: &Path) -> String {
    stdout(&git(
        dir,
        &["-c", "status.compareBranches=@{upstream} @{push}", "status"],
    ))
}

#[test]
fn a_behind_push_destination_names_its_remote_and_branch() {
    let dir = fixture("behind", "fork");
    assert_eq!(
        status_both(&dir),
        "On branch main\n\
         Your branch is behind 'origin/main' by 1 commit, and can be fast-forwarded.\n\
         \x20 (use \"git pull\" to update your local branch)\n\
         \n\
         Your branch is behind 'fork/main' by 1 commit, and can be fast-forwarded.\n\
         \x20 (use \"git pull fork main\" to update your local branch)\n\
         \n\
         nothing to commit, working tree clean\n"
    );
}

/// The push ref lives under `refs/remotes/mirror/`, which does not start with
/// the push remote's name, so `push_remote_name` is reset and no pull hint is
/// armed for that comparison.
#[test]
fn a_push_ref_outside_the_push_remotes_hierarchy_gets_no_pull_hint() {
    let dir = fixture("mirror", "mirror");
    assert_eq!(
        status_both(&dir),
        "On branch main\n\
         Your branch is behind 'origin/main' by 1 commit, and can be fast-forwarded.\n\
         \x20 (use \"git pull\" to update your local branch)\n\
         \n\
         Your branch is behind 'mirror/main' by 1 commit, and can be fast-forwarded.\n\
         \n\
         nothing to commit, working tree clean\n"
    );
}

/// The divergence hint is gated on `is_upstream` alone, so the push comparison
/// stays bare even though `push_remote_name` was found.
#[test]
fn a_diverged_push_destination_still_gets_no_hint() {
    let dir = fixture("diverged", "fork");
    git(&dir, &["commit", "-q", "--allow-empty", "-m", "local"]);
    assert_eq!(
        status_both(&dir),
        "On branch main\n\
         Your branch and 'origin/main' have diverged,\n\
         and have 1 and 1 different commits each, respectively.\n\
         \x20 (use \"git pull\" if you want to integrate the remote branch with yours)\n\
         \n\
         Your branch and 'fork/main' have diverged,\n\
         and have 1 and 1 different commits each, respectively.\n\
         \n\
         nothing to commit, working tree clean\n"
    );
}
