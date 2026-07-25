//! `git push <remote> <refspec> --tags` pushes both.
//!
//! git documents `--tags` as "all refs under refs/tags are pushed, in addition
//! to refspecs explicitly listed on the command line" — so the combination is
//! the ordinary release command, `git push origin main --tags`. It was rejected
//! outright with "--tags can't be combined with refspecs", which is the rule for
//! `--all`, not for `--tags`.
//!
//! The remote is a bare repo on disk, so nothing here touches a network.

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

fn out_text(out: &Output) -> String {
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    s
}

/// A bare remote plus a clone holding one commit, an annotated tag and a
/// lightweight one — neither kind may be left behind.
fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-pushtags-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let bare = root.join("bare");
    let work = root.join("work");
    std::fs::create_dir_all(&home).expect("mkdir home");
    std::fs::create_dir_all(&root).expect("mkdir root");

    assert!(
        zvcs(&root, &home, &["init", "-q", "--bare", bare.to_str().expect("utf-8")]).status.success(),
        "init bare"
    );
    assert!(
        zvcs(&root, &home, &["clone", "-q", bare.to_str().expect("utf-8"), "work"]).status.success(),
        "clone"
    );
    assert!(zvcs(&work, &home, &["checkout", "-q", "-B", "main"]).status.success(), "branch");
    assert!(
        zvcs(&work, &home, &["commit", "--allow-empty", "-q", "-m", "c0"]).status.success(),
        "commit"
    );
    assert!(zvcs(&work, &home, &["tag", "-a", "v1.0", "-m", "one"]).status.success(), "annotated");
    assert!(zvcs(&work, &home, &["tag", "light"]).status.success(), "lightweight");
    (root, work, bare)
}

/// Which refs exist on the remote, sorted. Read from inside the bare repo
/// rather than over a remote helper, so the assertion is about what the push
/// actually wrote.
fn remote_refs(home: &Path, bare: &Path) -> Vec<String> {
    let out = zvcs(bare, home, &["for-each-ref", "--format=%(refname)"]);
    let mut refs: Vec<String> =
        String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect();
    refs.sort();
    refs
}

#[test]
fn a_refspec_and_tags_are_pushed_together() {
    let (root, work, bare) = fixture("both");
    let home = root.join("home");

    let out = zvcs(&work, &home, &["push", "origin", "main", "--tags"]);
    assert!(out.status.success(), "push refused the combination:\n{}", out_text(&out));

    let refs = remote_refs(&home, &bare);
    assert!(refs.iter().any(|r| r == "refs/heads/main"), "the named refspec was pushed: {refs:?}");
    assert!(refs.iter().any(|r| r == "refs/tags/v1.0"), "the annotated tag was pushed: {refs:?}");
    assert!(refs.iter().any(|r| r == "refs/tags/light"), "the lightweight tag was pushed: {refs:?}");

    // git lists the explicit refspec before the tags it added.
    let text = out_text(&out);
    let branch_at = text.find("main -> main").expect("branch line");
    let tag_at = text.find("v1.0 -> v1.0").expect("tag line");
    assert!(branch_at < tag_at, "the named ref is reported before the added tags:\n{text}");
}

/// `--tags` alone still pushes only tags — the branch stays behind.
#[test]
fn tags_alone_pushes_no_branch() {
    let (root, work, bare) = fixture("alone");
    let home = root.join("home");

    assert!(zvcs(&work, &home, &["push", "origin", "--tags"]).status.success(), "push --tags");

    let refs = remote_refs(&home, &bare);
    assert!(refs.iter().any(|r| r == "refs/tags/v1.0"), "tags pushed: {refs:?}");
    assert!(!refs.iter().any(|r| r == "refs/heads/main"), "no branch pushed: {refs:?}");
}

/// `--all` really is exclusive with a refspec, and must stay that way — the fix
/// above is about `--tags` only.
#[test]
fn all_still_refuses_a_refspec() {
    let (root, work, _bare) = fixture("all");
    let home = root.join("home");

    let out = zvcs(&work, &home, &["push", "origin", "main", "--all"]);
    assert!(!out.status.success(), "--all with a refspec must still be refused");
    assert!(
        out_text(&out).contains("can't be combined with refspecs"),
        "and say why:\n{}",
        out_text(&out)
    );
}
