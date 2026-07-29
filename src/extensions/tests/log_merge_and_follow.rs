//! `git log --follow` and the `--diff-merges` family, pinned against stock git
//! 2.50.1.
//!
//! The traps here are all in *when* a diff appears and what it is limited to:
//!
//!   * `-c`/`--cc` turn the patch on by themselves, but only when no other format
//!     claimed it — `-c --stat` stays a stat;
//!   * a merge's combined diff is separated from the header even under `oneline`,
//!     which is the one format that otherwise runs the patch straight on;
//!   * `--name-only`/`--name-status` under `-c` report the *combined* pair list
//!     while the stat formats stay on the first-parent diff;
//!   * `-m` does *not* imply a patch, so `git log -m` alone prints no diff;
//!   * `--follow` limits each commit by the name the file had *at that commit*, so
//!     the batching used for ordinary patches cannot serve it.

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

fn scratch(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-logmf-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &home, &["init", "-q", "-b", "main"]);
    (root, repo, home)
}

/// A merge that resolved a conflict, so the combined diff has something to show.
fn conflicted_merge(repo: &Path, home: &Path) {
    std::fs::write(repo.join("f"), "a\nb\nc\n").unwrap();
    git(repo, home, &["add", "f"]);
    git(repo, home, &["commit", "-q", "-m", "base"]);
    git(repo, home, &["checkout", "-q", "-b", "side"]);
    std::fs::write(repo.join("f"), "a\nSIDE\nc\n").unwrap();
    std::fs::write(repo.join("g"), "s\n").unwrap();
    git(repo, home, &["add", "-A"]);
    git(repo, home, &["commit", "-q", "-m", "side"]);
    git(repo, home, &["checkout", "-q", "main"]);
    std::fs::write(repo.join("f"), "a\nMAIN\nc\n").unwrap();
    git(repo, home, &["commit", "-q", "-a", "-m", "main2"]);
    // Merge, resolve, commit — the resolution is what makes the combined diff
    // non-empty.
    let _ = run(repo, home, &["merge", "side"]);
    std::fs::write(repo.join("f"), "a\nMERGED\nc\n").unwrap();
    git(repo, home, &["add", "f"]);
    git(repo, home, &["commit", "-q", "-m", "merge side"]);
}

#[test]
fn combined_merge_diffs_match_git() {
    let (root, repo, home) = scratch("merges");
    conflicted_merge(&repo, &home);

    // A merge gets no diff by default, whatever the format asks for.
    assert_eq!(out(&repo, &home, &["log", "-1", "-p", "--format=%h"]).lines().count(), 1);

    // `-c` turns the patch on by itself and heads it `diff --combined`; `--cc`
    // heads the same body `diff --cc`.
    let c = out(&repo, &home, &["log", "-1", "-c", "--format=%h"]);
    assert!(c.contains("\ndiff --combined f\n"), "{c}");
    assert!(c.contains("@@@ -1,3 -1,3 +1,3 @@@\n"), "{c}");
    let cc = out(&repo, &home, &["log", "-1", "--cc", "--format=%h"]);
    assert_eq!(cc, c.replace("diff --combined f", "diff --cc f"));

    // Under `oneline` the combined diff is still separated by a blank line, unlike
    // an ordinary patch.
    let one = out(&repo, &home, &["log", "-1", "-c", "--oneline"]);
    let mut lines = one.lines();
    assert!(lines.next().is_some_and(|l| l.ends_with("merge side")), "{one}");
    assert_eq!(lines.next(), Some(""), "a combined diff is fenced off even here: {one}");

    // `-c --stat` stays a stat: the patch is implied only when nothing else
    // claimed the format, and the stat is the first-parent one.
    let stat = out(&repo, &home, &["log", "-1", "-c", "--stat", "--format=%h"]);
    assert!(!stat.contains("diff --combined"), "{stat}");
    assert!(stat.contains(" g | 1 +\n"), "the stat is against the first parent: {stat}");

    // The name formats *do* switch to the combined pair list, with one status
    // letter per parent — `g` matches the side parent, so only `f` is listed.
    assert_eq!(
        out(&repo, &home, &["log", "-1", "-c", "--name-status", "--format=%h"])
            .lines()
            .filter(|l| l.contains('\t'))
            .collect::<Vec<_>>(),
        vec!["MM\tf"]
    );

    // `-m` implies no patch at all.
    assert_eq!(out(&repo, &home, &["log", "-1", "-m", "--format=%h"]).lines().count(), 1);

    let bad = run(&repo, &home, &["log", "-1", "--diff-merges=nope"]);
    assert_eq!(bad.status.code(), Some(128));
    assert_eq!(
        String::from_utf8_lossy(&bad.stderr),
        "fatal: invalid value for '--diff-merges': 'nope'\n"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// `--follow` walks back through every name the file has had, and limits each
/// commit's diff to the name it had there.
#[test]
fn follow_tracks_a_file_across_renames() {
    let (root, repo, home) = scratch("follow");
    std::fs::write(repo.join("old.txt"), "l1\nl2\nl3\nl4\nl5\n").unwrap();
    git(&repo, &home, &["add", "-A"]);
    git(&repo, &home, &["commit", "-q", "-m", "add old.txt"]);
    std::fs::write(repo.join("old.txt"), "l1\nl2\nCHANGED\nl4\nl5\n").unwrap();
    git(&repo, &home, &["commit", "-q", "-a", "-m", "edit old.txt"]);
    git(&repo, &home, &["mv", "old.txt", "mid.txt"]);
    git(&repo, &home, &["commit", "-q", "-m", "rename old->mid"]);
    git(&repo, &home, &["mv", "mid.txt", "new.txt"]);
    std::fs::write(repo.join("new.txt"), "l1\nl2\nCHANGED\nl4\nl5\nl6\n").unwrap();
    git(&repo, &home, &["add", "-A"]);
    git(&repo, &home, &["commit", "-q", "-m", "rename mid->new and edit"]);
    std::fs::write(repo.join("other.txt"), "unrelated\n").unwrap();
    git(&repo, &home, &["add", "-A"]);
    git(&repo, &home, &["commit", "-q", "-m", "unrelated"]);

    // Without `--follow` the log stops at the last rename.
    let plain: Vec<String> = out(&repo, &home, &["log", "--format=%s", "--", "new.txt"])
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(plain, vec!["rename mid->new and edit"]);

    // With it, every name the file had is walked.
    let followed: Vec<String> = out(&repo, &home, &["log", "--follow", "--format=%s", "--", "new.txt"])
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(
        followed,
        vec![
            "rename mid->new and edit",
            "rename old->mid",
            "edit old.txt",
            "add old.txt",
        ]
    );

    // Each commit's diff is limited to the name the file had *there*: the older
    // commits' patches name `old.txt`, which the command line never mentioned.
    let patch = out(&repo, &home, &["log", "--follow", "-p", "--format=%h", "--", "new.txt"]);
    assert!(patch.contains("diff --git a/old.txt b/old.txt\n"), "{patch}");
    assert!(patch.contains("+CHANGED\n"), "{patch}");

    // One pathspec only.
    let two = run(&repo, &home, &["log", "--follow", "--oneline", "--", "new.txt", "other.txt"]);
    assert_eq!(two.status.code(), Some(128));
    assert_eq!(
        String::from_utf8_lossy(&two.stderr),
        "fatal: --follow requires exactly one pathspec\n"
    );

    let _ = std::fs::remove_dir_all(&root);
}
