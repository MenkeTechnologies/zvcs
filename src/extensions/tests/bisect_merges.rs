//! `git bisect` over history that contains merges, pinned against stock git
//! 2.50.1.
//!
//! Picking the midpoint is not "the middle of the list": `find_bisection()`
//! weights each candidate by how many candidates it reaches and returns the first
//! one that is halfway. Two details of that search decide which of several
//! equally good candidates gets tested, and both are easy to get wrong:
//!
//!   * the list is walked *oldest first* — `find_bisection()` reverses it while
//!     counting;
//!   * a commit seeded with weight 1 because it has no interesting parent is never
//!     offered to the halfway shortcut, which is why a three-commit range tests the
//!     middle commit and not the oldest.
//!
//! Both show up as a different commit checked out, with the same "N revisions
//! left" line — so a test that only checks the count would pass while the
//! bisection walks a different path.

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

/// The subject of the commit `bisect` just checked out, taken from its own
/// `[<oid>] <subject>` line.
fn tested_subject(text: &str) -> String {
    text.lines()
        .find(|l| l.starts_with('['))
        .and_then(|l| l.split_once("] "))
        .map(|(_, s)| s.to_string())
        .unwrap_or_default()
}

fn commit(dir: &Path, home: &Path, file: &str, body: &str, message: &str) {
    std::fs::write(dir.join(file), format!("{body}\n")).unwrap();
    git(dir, home, &["add", file]);
    git(dir, home, &["commit", "-q", "-m", message]);
}

/// A history shaped like the one bisect is actually used on: a mainline, a side
/// branch merged back, and more mainline after it.
fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-bisect-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &home, &["init", "-q", "-b", "main"]);
    commit(&repo, &home, "f", "base", "c0");
    commit(&repo, &home, "f", "m1", "m1");
    git(&repo, &home, &["checkout", "-q", "-b", "side"]);
    commit(&repo, &home, "g", "s1", "s1");
    commit(&repo, &home, "g", "s2", "s2");
    git(&repo, &home, &["checkout", "-q", "main"]);
    commit(&repo, &home, "f", "m2", "m2");
    git(&repo, &home, &["merge", "-q", "--no-ff", "-m", "merge side", "side"]);
    (root, repo, home)
}

/// A range that spans a merge is bisected rather than refused, and the commit it
/// picks is the one stock git picks.
#[test]
fn bisect_crosses_a_merge() {
    let (root, repo, home) = fixture("merge");
    let root_commit = out(&repo, &home, &["rev-list", "--max-parents=0", "HEAD"]).trim().to_string();

    let start = run(&repo, &home, &["bisect", "start", "HEAD", &root_commit]);
    assert!(start.status.success(), "{}", String::from_utf8_lossy(&start.stderr));
    let text = String::from_utf8_lossy(&start.stdout);
    assert!(
        text.starts_with("Bisecting: 2 revisions left to test after this (roughly 1 step)\n"),
        "{text}"
    );
    // Five candidates: m1, s1, s2, m2, merge. Their weights are 1, 2, 3, 2 and 5,
    // so the halfway commits are the two of weight 2 and 3 — and the oldest-first
    // walk reaches s1 first, skipping m1 because it was seeded rather than
    // derived.
    assert_eq!(tested_subject(&text), "s1", "{text}");

    // Answering good walks into the other half, and the session ends on a commit
    // rather than refusing anywhere along the way.
    let step = run(&repo, &home, &["bisect", "good"]);
    assert!(step.status.success(), "{}", String::from_utf8_lossy(&step.stderr));
    let step = String::from_utf8_lossy(&step.stdout);
    assert!(step.starts_with("Bisecting: "), "{step}");

    let _ = std::fs::remove_dir_all(&root);
}

/// The seeded-weight rule, isolated: in a three-commit range both the oldest
/// (weight 1) and the middle (weight 2) are halfway by the arithmetic, and git
/// tests the middle one because the oldest never reaches the shortcut.
#[test]
fn three_commit_range_tests_the_middle_commit() {
    let (root, repo, home) = fixture("three");
    // `side` holds c0, m1, s1, s2; bisecting it against m1 leaves s1 and s2 …
    let start = run(&repo, &home, &["bisect", "start", "side", "side~2"]);
    assert!(start.status.success(), "{}", String::from_utf8_lossy(&start.stderr));
    let text = String::from_utf8_lossy(&start.stdout);
    assert!(
        text.starts_with("Bisecting: 0 revisions left to test after this (roughly 0 steps)\n"),
        "{text}"
    );
    assert_eq!(tested_subject(&text), "s1", "{text}");
    let _ = std::fs::remove_dir_all(&root);
}
