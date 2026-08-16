//! Which refs `git bundle create` writes into the header when the command line
//! also excludes something.
//!
//! `write_bundle_refs()` (`bundle.c`) skips a pending entry on `e->item->flags &
//! UNINTERESTING` — the flag of the *object*, not of the entry. By the time the
//! header is written, the walk that produced the prerequisites has already spread
//! that flag from every `^<rev>` to all of its ancestors, so a ref is dropped as
//! soon as anything excluded reaches the commit it points at. It is not enough for
//! the same name to have been mentioned twice.
//!
//! Two consequences the assertions below pin down:
//!
//! * `--all ^main` keeps only the refs outside `main`'s history, even though every
//!   one of them was named positively by `--all`;
//! * an annotated tag survives that exclusion, because its pending object is the
//!   tag object and a tag object is not an ancestor of anything.
//!
//! Every expectation was checked against stock git 2.50.1 in the same fixture
//! before being written down.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Run `git <args>` in `repo`, returning stdout, stderr and the exit code.
///
/// The identity and both timestamps are pinned so the fixture's object ids are the
/// same on every machine and every run.
fn git(repo: &Path, home: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(repo)
        .env("ZVCS_HOME", home)
        .env("HOME", home)
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "2005-04-07T22:13:13 +0200")
        .env("GIT_COMMITTER_DATE", "2005-04-07T22:13:13 +0200")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

/// `git <args>`, failing loudly on a non-zero exit — for fixture construction,
/// where a partial success would silently weaken the premise.
fn must(repo: &Path, home: &Path, args: &[&str]) {
    let (_, stderr, code) = git(repo, home, args);
    assert_eq!(code, 0, "git {args:?} failed: {stderr}");
}

/// Two commits on `main`, a lightweight and an annotated tag on its tip, and a
/// `feature` branch one commit ahead.
///
/// `main` is an ancestor of `feature`, and the lightweight tag names the same
/// commit as `main` while the annotated one names a tag object — the three cases
/// `--all ^<rev>` has to tell apart.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-bundle-uninteresting-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let root = root.canonicalize().unwrap();
    let repo = repo.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();

    must(&repo, &home, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "# fixture\n").unwrap();
    must(&repo, &home, &["add", "README.md"]);
    must(&repo, &home, &["commit", "-qm", "initial commit"]);
    std::fs::write(repo.join("two.txt"), "two\n").unwrap();
    must(&repo, &home, &["add", "two.txt"]);
    must(&repo, &home, &["commit", "-qm", "add two"]);
    must(&repo, &home, &["tag", "v0.1.0"]);
    must(&repo, &home, &["tag", "-a", "v0.2.0", "-m", "annotated"]);
    must(&repo, &home, &["branch", "feature"]);
    must(&repo, &home, &["checkout", "-q", "feature"]);
    std::fs::write(repo.join("feature.txt"), "feature work\n").unwrap();
    must(&repo, &home, &["add", "feature.txt"]);
    must(&repo, &home, &["commit", "-qm", "feature commit"]);
    must(&repo, &home, &["checkout", "-q", "main"]);

    (root, repo)
}

/// The `<oid> <name>` lines of the bundle's header, in the order they were written.
fn heads(repo: &Path, home: &Path, bundle: &Path) -> Vec<String> {
    let path = bundle.to_str().unwrap();
    let (stdout, stderr, code) = git(repo, home, &["bundle", "list-heads", path]);
    assert_eq!(code, 0, "bundle list-heads: {stderr}");
    stdout.lines().map(str::to_string).collect()
}

/// The full object id of `rev`.
fn oid(repo: &Path, home: &Path, rev: &str) -> String {
    let (stdout, stderr, code) = git(repo, home, &["rev-parse", rev]);
    assert_eq!(code, 0, "rev-parse {rev}: {stderr}");
    stdout.trim().to_string()
}

/// `--all` names every ref positively, and `^main` still removes every ref whose
/// commit `main` reaches: `main` itself, `HEAD`, and the lightweight tag that
/// shares its commit. `feature` is ahead of `main` and stays; the annotated tag
/// stays because the object it puts in the pending list is a tag, which the
/// exclusion never reaches.
#[test]
fn all_drops_every_ref_the_exclusion_reaches() {
    let (root, repo) = fixture("all-minus-main");
    let home = root.join("home");
    let bundle = root.join("out.bundle");

    let (_, stderr, code) = git(
        &repo,
        &home,
        &["bundle", "create", bundle.to_str().unwrap(), "--all", "^main"],
    );
    assert_eq!(code, 0, "stderr: {stderr}");

    assert_eq!(
        heads(&repo, &home, &bundle),
        vec![
            format!("{} refs/heads/feature", oid(&repo, &home, "feature")),
            format!("{} refs/tags/v0.2.0", oid(&repo, &home, "v0.2.0")),
        ]
    );
}

/// The whole history excluded leaves only the annotated tag object, which is not
/// a commit and so never inherits the flag.
#[test]
fn only_the_tag_object_survives_excluding_the_tip() {
    let (root, repo) = fixture("all-minus-feature");
    let home = root.join("home");
    let bundle = root.join("out.bundle");

    let (_, stderr, code) = git(
        &repo,
        &home,
        &["bundle", "create", bundle.to_str().unwrap(), "--all", "^feature"],
    );
    assert_eq!(code, 0, "stderr: {stderr}");

    assert_eq!(
        heads(&repo, &home, &bundle),
        vec![format!("{} refs/tags/v0.2.0", oid(&repo, &home, "v0.2.0"))]
    );
}

/// One commit named both ways leaves nothing to describe, and git refuses to
/// write a bundle with an empty ref list rather than writing an empty one.
#[test]
fn a_ref_named_both_ways_leaves_an_empty_bundle_refused() {
    let (root, repo) = fixture("both-ways");
    let home = root.join("home");
    let bundle = root.join("out.bundle");

    let (_, stderr, code) = git(
        &repo,
        &home,
        &["bundle", "create", bundle.to_str().unwrap(), "main", "^main"],
    );

    assert_eq!(code, 128, "stderr: {stderr}");
    assert_eq!(stderr, "fatal: Refusing to create empty bundle.\n");
    assert!(!bundle.exists(), "no bundle file is left behind");
}

/// The control: exclusion must not become over-exclusion. `feature ^main` keeps
/// the one ref whose commit `main` does not reach.
#[test]
fn a_ref_outside_the_exclusion_is_kept() {
    let (root, repo) = fixture("control");
    let home = root.join("home");
    let bundle = root.join("out.bundle");

    let (_, stderr, code) = git(
        &repo,
        &home,
        &["bundle", "create", bundle.to_str().unwrap(), "feature", "^main"],
    );
    assert_eq!(code, 0, "stderr: {stderr}");

    assert_eq!(
        heads(&repo, &home, &bundle),
        vec![format!("{} refs/heads/feature", oid(&repo, &home, "feature"))]
    );
}
