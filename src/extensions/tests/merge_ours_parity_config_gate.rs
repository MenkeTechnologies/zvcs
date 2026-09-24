//! `cmd_merge_ours` is `show_usage_if_asked()`, then
//! `repo_config(repo, git_default_config, NULL)`, then `prepare_repo_settings()`
//! (builtin/merge-ours.c:25-28). The port read no configuration at all, so
//! `git -c color.advice=bogus merge-ours` compared the index and exited 0 where
//! git dies at 128. The order matters too: the default callback runs before the
//! settings block, so it reports first whatever the order on the command line.
//!
//! Every expectation was measured against stock git 2.55.0.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new(BIN).args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

fn run(repo: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(repo)
        .env("HOME", repo)
        .env("ZVCS_HOME", repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn fixture(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-oursgate-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let repo = root.canonicalize().unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@e.x"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("f"), "f\n").unwrap();
    git(&repo, &["add", "f"]);
    git(&repo, &["commit", "-qm", "one"]);
    repo
}

fn assert_dies(repo: &Path, args: &[&str], stderr: &str) {
    let out = run(repo, args);
    assert_eq!(out.status.code(), Some(128), "{args:?}");
    assert_eq!(String::from_utf8_lossy(&out.stderr), stderr, "{args:?}");
}

#[test]
fn the_default_callback_refuses_before_the_settings_block() {
    let repo = fixture("order");
    assert_dies(
        &repo,
        &["-c", "color.advice=bogus", "-c", "color.advice=false", "merge-ours"],
        "fatal: bad boolean config value 'bogus' for 'color.advice'\n",
    );
    let object_mode = "fatal: invalid mode for object creation: bogus\n";
    assert_dies(
        &repo,
        &["-c", "index.version=bogus", "-c", "core.createObject=bogus", "merge-ours"],
        object_mode,
    );
    assert_dies(
        &repo,
        &["-c", "core.createObject=bogus", "-c", "index.version=bogus", "merge-ours"],
        object_mode,
    );
    // Alone, the settings key still refuses.
    assert_dies(
        &repo,
        &["-c", "index.version=bogus", "merge-ours"],
        "fatal: bad numeric config value 'bogus' for 'index.version': invalid unit\n",
    );
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn help_is_answered_before_any_configuration() {
    let repo = fixture("help");
    let out = run(&repo, &["-c", "color.advice=bogus", "merge-ours", "-h"]);
    assert_eq!(out.status.code(), Some(129));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "usage: git merge-ours <base>... -- HEAD <remote>...\n"
    );
    assert!(out.stderr.is_empty());
    // A clean configuration still compares the index: it matches `HEAD`.
    assert_eq!(run(&repo, &["merge-ours"]).status.code(), Some(0));
    let _ = std::fs::remove_dir_all(&repo);
}
