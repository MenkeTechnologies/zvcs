//! `git log <annotated-tag>` walks from the commit the tag points at.
//!
//! A tag object is not a walkable node, so the walk has to peel it first. It did
//! not, and every release tag — the most natural thing anyone types after a
//! release — failed outright with "was supposed to be of kind commit, but was
//! kind tag". A lightweight tag was unaffected, which is why it survived: the
//! tags in most test fixtures are lightweight.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn zvcs(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .expect("run zvcs git")
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Three commits, an annotated tag on the second, a lightweight tag on the third.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-tagwalk-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    let home = root.join("home");
    std::fs::create_dir_all(&repo).expect("mkdir repo");
    std::fs::create_dir_all(&home).expect("mkdir home");
    assert!(zvcs(&repo, &home, &["init", "-q", "-b", "main"]).status.success(), "init");

    for msg in ["first", "second", "third"] {
        assert!(
            zvcs(&repo, &home, &["commit", "--allow-empty", "-q", "-m", msg]).status.success(),
            "commit {msg}"
        );
        if msg == "second" {
            assert!(
                zvcs(&repo, &home, &["tag", "-a", "v1.0", "-m", "release one"]).status.success(),
                "annotated tag"
            );
        }
    }
    assert!(zvcs(&repo, &home, &["tag", "light"]).status.success(), "lightweight tag");
    (repo, home)
}

/// The walk starts at the tagged commit, and the commits after it are not shown.
#[test]
fn an_annotated_tag_walks_from_the_commit_it_points_at() {
    let (repo, home) = fixture("annotated");

    let out = zvcs(&repo, &home, &["log", "--format=%s", "v1.0"]);
    assert!(out.status.success(), "log <annotated tag>: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(stdout_of(&out), "second\nfirst\n");
}

/// A lightweight tag names the commit directly and must behave identically.
#[test]
fn a_lightweight_tag_walks_the_same_way() {
    let (repo, home) = fixture("light");

    let out = zvcs(&repo, &home, &["log", "--format=%s", "light"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(stdout_of(&out), "third\nsecond\nfirst\n");
}

/// A range endpoint is peeled too — `<tag>..<rev>` is how a release diff is read.
#[test]
fn a_tag_works_as_a_range_endpoint() {
    let (repo, home) = fixture("range");

    let out = zvcs(&repo, &home, &["log", "--format=%s", "v1.0..HEAD"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(stdout_of(&out), "third\n");
}
