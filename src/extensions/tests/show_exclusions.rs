//! `git show` and the exclusion side of the revision grammar.
//!
//! `cmd_show` starts with `rev.no_walk = 1` and prints its pending objects one by
//! one, but `add_pending_object_with_path()` clears that flag the moment an object
//! carrying `UNINTERESTING` is added. Every spelling of an exclusion therefore turns
//! the whole invocation into `cmd_log_walk` — `^<rev>`, the left side of `A..B`, the
//! merge bases of `A...B`, `--not`, and the parents of `<rev>^!` — and the exclusion
//! applies to the other arguments rather than to a walk they never entered.
//!
//! The case that named this file is `git show ^HEAD HEAD~2 HEAD^0`: stock git prints
//! nothing, because `^HEAD` excludes everything the other two operands name. zvcs
//! printed the root commit, having stopped the hidden painting as soon as it first
//! met the overlap, with `HEAD~2` still unpainted.
//!
//! Every expectation here was taken from stock `git` 2.55 on the same fixture. All
//! commits share one commit date (git's classic `1136214245`), which is what makes
//! the frontier painting order-sensitive and the regression reproducible.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// One fixed author/committer date for the whole history.
const DATE: &str = "1136214245 +0000";

fn run(cwd: &Path, home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_DATE", DATE)
        .env("GIT_COMMITTER_DATE", DATE)
        .output()
        .expect("run binary")
}

/// The commit subjects `git show --oneline -s <args>` prints, in order.
fn subjects(repo: &Path, home: &Path, args: &[&str]) -> Vec<String> {
    let mut a = vec!["show", "--oneline", "-s"];
    a.extend_from_slice(args);
    let out = run(repo, home, &a);
    assert_eq!(out.status.code(), Some(0), "`show {args:?}` must succeed");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_once(' ').map(|(_, s)| s.to_string()))
        .collect()
}

/// ```text
/// *   m      (main, HEAD)
/// |\
/// | * s1     (side)
/// * | c2
/// |/
/// * c1       (v1, an annotated tag)
/// * c0
/// ```
///
/// So `HEAD^1` is `c2`, `HEAD^2` is `s1`, and `HEAD~2` is `c1`.
fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-showexcl-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();

    let commit = |msg: &str, path: &str| {
        std::fs::write(repo.join(path), format!("{msg}\n")).unwrap();
        run(&repo, &home, &["add", path]);
        run(&repo, &home, &["commit", "-q", "-m", msg]);
    };

    run(&repo, &home, &["init", "-q", "-b", "main"]);
    run(&repo, &home, &["config", "user.email", "t@e.co"]);
    run(&repo, &home, &["config", "user.name", "t"]);
    commit("c0", "f");
    commit("c1", "f");
    run(&repo, &home, &["tag", "-a", "v1", "-m", "tag c1"]);
    commit("c2", "f");
    run(&repo, &home, &["checkout", "-q", "-b", "side", "HEAD~1"]);
    commit("s1", "g");
    run(&repo, &home, &["checkout", "-q", "main"]);
    run(&repo, &home, &["merge", "-q", "--no-ff", "-m", "m", "side"]);

    (root, repo, home)
}

/// `^<rev>` excludes the commit *and its ancestors* from everything else the
/// command line names, however the other operands spell them.
#[test]
fn caret_excludes_the_other_operands() {
    let (root, repo, home) = fixture("caret");

    // The reported case: `^HEAD` covers `HEAD~2` (an ancestor) and `HEAD^0`
    // (the same commit under another name), so nothing is left to print.
    assert!(subjects(&repo, &home, &["^HEAD", "HEAD~2", "HEAD^0"]).is_empty());
    // The same commit excluded and named, in either order.
    assert!(subjects(&repo, &home, &["^HEAD", "HEAD"]).is_empty());
    assert!(subjects(&repo, &home, &["HEAD", "^HEAD"]).is_empty());
    // A partial exclusion still walks: `^HEAD~1` hides `c2` and below, leaving the
    // merge and the branch lane it did not reach.
    assert_eq!(subjects(&repo, &home, &["^HEAD~1", "HEAD"]), ["m", "s1"]);
    // A `^` on a branch name, hiding one lane of the merge.
    assert_eq!(subjects(&repo, &home, &["^side", "main"]), ["m", "c2"]);

    let _ = std::fs::remove_dir_all(root);
}

/// An annotated tag is peeled on both sides of the exclusion: `check_single_commit`
/// derefs it and `handle_commit` walks the tag chain, so `^v1` hides the commit `v1`
/// names and `v1` as a tip walks from that commit rather than printing the tag.
#[test]
fn annotated_tags_are_peeled_for_the_walk() {
    let (root, repo, home) = fixture("tags");

    assert_eq!(subjects(&repo, &home, &["^v1", "HEAD"]), ["m", "c2", "s1"]);
    // The range spelling of the same thing.
    assert_eq!(subjects(&repo, &home, &["v1..HEAD"]), ["m", "c2", "s1"]);
    // Named and excluded at once: the tag never prints its own header here, because
    // the exclusion turned the command into a walk.
    assert!(subjects(&repo, &home, &["v1", "^v1"]).is_empty());
    assert!(subjects(&repo, &home, &["^main", "v1"]).is_empty());
    // Without an exclusion the tag object itself is what `show` prints.
    let out = run(&repo, &home, &["show", "-s", "--oneline", "v1"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.starts_with("tag v1\n"), "plain `show v1` prints the tag: {text:?}");

    let _ = std::fs::remove_dir_all(root);
}

/// A pending object that is not a commit is an object for `--objects` to list, never
/// a commit the walk can start from, so it contributes nothing once a `^` is present.
#[test]
fn non_commit_operands_drop_out_of_the_walk() {
    let (root, repo, home) = fixture("noncommit");

    assert!(subjects(&repo, &home, &["^HEAD", "HEAD^{tree}"]).is_empty());
    assert!(subjects(&repo, &home, &["^HEAD", "HEAD:f"]).is_empty());
    // A lone `^<rev>` leaves no positive operand at all, and the implicit `HEAD`
    // fallback only applies when nothing was named.
    assert!(subjects(&repo, &home, &["^HEAD"]).is_empty());

    let _ = std::fs::remove_dir_all(root);
}

/// `--not` is the same `UNINTERESTING` flip as `^`, applied to every revision after
/// it and undone by a second `--not`.
#[test]
fn not_flips_the_revisions_after_it() {
    let (root, repo, home) = fixture("not");

    // `--not HEAD~1 HEAD` excludes both, leaving nothing.
    assert!(subjects(&repo, &home, &["--not", "HEAD~1", "HEAD"]).is_empty());
    // The toggle only applies to what follows it.
    assert_eq!(subjects(&repo, &home, &["HEAD", "--not", "HEAD~1"]), ["m", "s1"]);
    // `--not ^rev` cancels back to a positive revision, which keeps `no_walk` and
    // prints the commit itself.
    assert_eq!(subjects(&repo, &home, &["--not", "^HEAD"]), ["m"]);
    // A second `--not` restores the positive reading.
    assert_eq!(subjects(&repo, &home, &["--not", "HEAD~1", "--not", "HEAD"]), ["m", "s1"]);
    // A trailing `--not` with nothing after it never turns the implicit `HEAD` into
    // an exclusion.
    assert_eq!(subjects(&repo, &home, &["--not"]), ["m"]);

    let _ = std::fs::remove_dir_all(root);
}

/// `<rev>^@` adds the parents positively, which leaves `no_walk` alone, while
/// `<rev>^!` adds them excluded, which does not.
#[test]
fn parent_shorthands_take_the_right_side() {
    let (root, repo, home) = fixture("parents");

    // `^@`: the two parents themselves, not their history.
    assert_eq!(subjects(&repo, &home, &["HEAD^@"]), ["c2", "s1"]);
    // Peeled through the tag, exactly as `add_parents_only()` does.
    assert_eq!(subjects(&repo, &home, &["v1^@"]), ["c0"]);
    // `^!`: the commit with its parents excluded.
    assert_eq!(subjects(&repo, &home, &["HEAD^!"]), ["m"]);
    // …and the exclusion is visible to a second argument that names those parents.
    assert_eq!(subjects(&repo, &home, &["HEAD^!", "HEAD^@"]), ["m"]);
    // `--not` swaps both readings.
    assert!(subjects(&repo, &home, &["--not", "HEAD^@"]).is_empty());

    let _ = std::fs::remove_dir_all(root);
}
