//! `git worktree add`'s `ORIG_HEAD` reflog under `core.logAllRefUpdates=always`.
//!
//! `add_worktree()` checks the new worktree out with a `reset --hard` child
//! (`builtin/worktree.c:396-406`), whose `reset_refs()` (`builtin/reset.c:301-322`)
//! points `ORIG_HEAD` at the resolved `HEAD`. The files backend logs that update
//! only when `core.logAllRefUpdates` is `always`, and the child reads the
//! `config.worktree` that `copy_filtered_worktree_config()` copied just before.
//! The message is `set_reflog_message()`'s (`builtin/reset.c:225-237`):
//! `reset: updating ORIG_HEAD`, or `$GIT_REFLOG_ACTION: updating ORIG_HEAD`.
//! Expected values were measured against stock git 2.56.0.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

const PINNED: [&str; 7] = [
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
    "GIT_AUTHOR_DATE",
    "GIT_COMMITTER_DATE",
    "GIT_REFLOG_ACTION",
];

fn git_env(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut c = Command::new(BIN);
    for v in PINNED {
        c.env_remove(v);
    }
    c.args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "2023-01-01 00:00:00 +0000")
        .env("GIT_COMMITTER_DATE", "2023-01-01 00:00:00 +0000");
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().unwrap()
}

fn ok(dir: &Path, args: &[&str]) -> String {
    ok_env(dir, args, &[])
}

fn ok_env(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
    let out = git_env(dir, args, env);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// A repository with one commit, `extensions.worktreeConfig` on, and the main
/// worktree's `config.worktree` carrying `core.logAllRefUpdates=<log_all>`.
fn repo(tag: &str, init_extra: &[&str], log_all: &str) -> (PathBuf, PathBuf, String) {
    let root = std::env::temp_dir().join(format!("zvcs-wtoh-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let r = root.join("r");
    std::fs::create_dir_all(&r).unwrap();
    let mut init = vec!["init", "-q", "-b", "main"];
    init.extend_from_slice(init_extra);
    init.push(".");
    ok(&r, &init);
    std::fs::write(r.join("f.txt"), "a\n").unwrap();
    ok(&r, &["add", "f.txt"]);
    ok(&r, &["commit", "-q", "-m", "base"]);
    ok(&r, &["config", "extensions.worktreeConfig", "true"]);
    ok(&r, &["config", "--worktree", "core.logAllRefUpdates", log_all]);
    let oid = ok(&r, &["rev-parse", "HEAD"]).trim().to_string();
    (root, r, oid)
}

fn zero(oid: &str) -> String {
    "0".repeat(oid.len())
}

#[test]
fn always_logs_orig_head_in_the_new_worktree() {
    let (root, r, oid) = repo("always", &[], "always");
    ok(&r, &["worktree", "add", "-q", "-b", "gen-wtb", "wt-gen"]);
    let log = std::fs::read_to_string(r.join(".git/worktrees/wt-gen/logs/ORIG_HEAD")).unwrap();
    assert_eq!(
        log,
        format!(
            "{} {oid} C O Mitter <committer@example.com> 1672531200 +0000\treset: updating ORIG_HEAD\n",
            zero(&oid)
        )
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn true_leaves_orig_head_unlogged() {
    let (root, r, _) = repo("true", &[], "true");
    ok(&r, &["worktree", "add", "-q", "-b", "gen-wtb", "wt-gen"]);
    assert!(r.join(".git/worktrees/wt-gen/ORIG_HEAD").is_file());
    assert!(!r.join(".git/worktrees/wt-gen/logs/ORIG_HEAD").exists());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn no_checkout_writes_no_orig_head_log() {
    let (root, r, _) = repo("nocheckout", &[], "always");
    ok(&r, &["worktree", "add", "-q", "--no-checkout", "-b", "gen-wtb", "wt-gen"]);
    assert!(!r.join(".git/worktrees/wt-gen/logs/ORIG_HEAD").exists());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn reflog_action_names_both_reset_entries() {
    let (root, r, oid) = repo("rla", &[], "always");
    ok_env(
        &r,
        &["worktree", "add", "-q", "-b", "gen-wtb", "wt-gen"],
        &[("GIT_REFLOG_ACTION", "foo")],
    );
    let wt = r.join("wt-gen");
    assert_eq!(
        ok(&wt, &["reflog", "show", "ORIG_HEAD", "--format=%H %gs"]),
        format!("{oid} foo: updating ORIG_HEAD\n")
    );
    assert_eq!(
        ok(&wt, &["reflog", "show", "HEAD", "--format=%H %gs"]),
        format!("{oid} foo: updating HEAD\n{oid} \n")
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn reftable_reads_the_copied_worktree_config() {
    let (root, r, oid) = repo("reftable", &["--ref-format=reftable"], "always");
    ok(&r, &["worktree", "add", "-q", "-b", "gen-wtb", "wt-gen"]);
    let wt = r.join("wt-gen");
    assert_eq!(
        ok(&wt, &["reflog", "show", "ORIG_HEAD", "--format=%H %gs"]),
        format!("{oid} reset: updating ORIG_HEAD\n")
    );
    assert_eq!(
        ok(&wt, &["reflog", "show", "HEAD", "--format=%H %gs"]),
        format!("{oid} reset: moving to HEAD\n{oid} \n")
    );
    let _ = std::fs::remove_dir_all(root);
}
