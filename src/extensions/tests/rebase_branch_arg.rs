//! `git rebase <upstream> <branch>` and the merge-commit handling of the todo
//! sheet, pinned against stock git 2.50.1.
//!
//! Three behaviours that only appear with a `<branch>` argument or a merge in the
//! range:
//!
//!   * `<branch>` is checked out first, silently — but *after*
//!     `require_clean_work_tree()`, so a dirty tree gets git's three-line refusal
//!     rather than the checkout's complaint;
//!   * `sequencer_make_script()` walks with `max_parents = 1`, so a merge inside
//!     the range never reaches the sheet — `pick` refuses a merge commit outright,
//!     which is what made `--onto` across a merged branch fail;
//!   * a commit whose patch is already upstream is dropped in the pick loop with
//!     `dropping <oid> <subject> -- patch contents already upstream`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .expect("run binary")
}

fn git(dir: &Path, home: &Path, args: &[&str]) {
    let o = run(dir, home, args);
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
}

fn out(dir: &Path, home: &Path, args: &[&str]) -> String {
    String::from_utf8_lossy(&run(dir, home, args).stdout).into_owned()
}

fn commit(dir: &Path, home: &Path, file: &str, body: &str, message: &str) {
    std::fs::write(dir.join(file), format!("{body}\n")).unwrap();
    git(dir, home, &["add", file]);
    git(dir, home, &["commit", "-q", "-m", message]);
}

/// `main` with a merged side branch, plus a `topic` branch to rebase.
fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-rebasearg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &home, &["init", "-q", "-b", "main"]);
    commit(&repo, &home, "f", "base", "c0");
    git(&repo, &home, &["checkout", "-q", "-b", "side"]);
    commit(&repo, &home, "g", "s1", "s1");
    git(&repo, &home, &["checkout", "-q", "main"]);
    commit(&repo, &home, "f", "m1", "m1");
    git(&repo, &home, &["merge", "-q", "--no-ff", "-m", "merge side", "side"]);
    git(&repo, &home, &["checkout", "-q", "-b", "topic", "main~1"]);
    commit(&repo, &home, "h", "t1", "t1");
    git(&repo, &home, &["checkout", "-q", "main"]);
    (root, repo, home)
}

/// The branch is checked out and rebased without leaving `HEAD` where it was.
#[test]
fn rebase_checks_out_the_named_branch() {
    let (root, repo, home) = fixture("switch");
    assert_eq!(out(&repo, &home, &["rev-parse", "--abbrev-ref", "HEAD"]).trim_end(), "main");

    let o = run(&repo, &home, &["rebase", "main", "topic"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("Successfully rebased and updated refs/heads/topic."),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    // git leaves the rebased branch checked out.
    assert_eq!(out(&repo, &home, &["rev-parse", "--abbrev-ref", "HEAD"]).trim_end(), "topic");
    // …and it now sits on top of main.
    let base = out(&repo, &home, &["merge-base", "main", "topic"]);
    assert_eq!(base.trim_end(), out(&repo, &home, &["rev-parse", "main"]).trim_end());
    assert!(out(&repo, &home, &["status", "--porcelain"]).is_empty());

    let _ = std::fs::remove_dir_all(&root);
}

/// A dirty tree is refused by `require_clean_work_tree()` — which runs *before*
/// the checkout, so the message is the rebase's, not the checkout's, and the
/// branch stays where it was.
#[test]
fn a_dirty_tree_is_refused_before_the_branch_is_checked_out() {
    let (root, repo, home) = fixture("dirty");
    std::fs::write(repo.join("f"), "dirtied\n").unwrap();

    let o = run(&repo, &home, &["rebase", "main", "topic"]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&o.stderr),
        "error: cannot rebase: You have unstaged changes.\nerror: Please commit or stash them.\n"
    );
    assert_eq!(
        out(&repo, &home, &["rev-parse", "--abbrev-ref", "HEAD"]).trim_end(),
        "main",
        "a refused rebase must not have switched branches"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// A merge inside the range is left out of the sheet rather than becoming a
/// `pick` the sequencer refuses, and a commit already upstream is dropped with
/// git's own wording.
#[test]
fn merges_are_not_picked_and_upstream_patches_are_dropped() {
    let (root, repo, home) = fixture("merge");
    // `main` contains the merge; rebasing it onto itself replays only the
    // non-merge commits, each of which is already upstream.
    let o = run(&repo, &home, &["rebase", "--onto", "main", "side"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        !err.contains("does not accept merge commits"),
        "a merge must never reach the sheet: {err}"
    );
    assert!(
        err.contains(" -- patch contents already upstream"),
        "an already-upstream commit is dropped with git's message: {err}"
    );

    let _ = std::fs::remove_dir_all(&root);
}
