//! `git fast-export`'s handling of a commit that is named both positively and
//! negatively on the same command line.
//!
//! `UNINTERESTING` is a flag on the *object*, not on the command-line entry that
//! mentioned it. `handle_revision_arg` sets it once through `add_pending_object`
//! (`revision.c`), and a later positive mention of the same commit — `--all`
//! naming the branch a `^rev` already excluded, or a plain `fast-export main
//! ^main` — appends a second pending entry without ever clearing it. So
//! `prepare_revision_walk` hands the commit to the walk still uninteresting: it
//! is not shown, and it does not keep its ancestors alive either.
//!
//! What still happens for such a ref is the *bookkeeping*:
//! `get_tags_and_duplicates` (`builtin/fast-export.c:1065`) skips on the entry's
//! own flags, so the `--all` entry survives into `extra_refs` and
//! `handle_tags_and_duplicates` prints `reset <ref>\nfrom <null-oid>` for it —
//! walking the name-sorted list backwards, before any tag is handled. When a tag
//! then aborts under `--tag-of-filtered-object=abort`, those resets are already
//! on stdout and the `die()` follows them.
//!
//! Every expectation below was checked against stock git 2.55.0 in the same
//! fixture before being written down.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Run `git <args>` in `repo`, returning stdout, stderr and the exit code.
///
/// The identity and both timestamps are pinned so the fixture's object ids are
/// the same on every machine and every run — the abort message names a tag by
/// id, and the test reads that id back out of the fixture rather than spelling
/// it out.
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

/// Two commits on `main`, a lightweight tag and an annotated tag on its tip, and
/// a `feature` branch one commit ahead of it.
///
/// The shape is the point: `main` is an *ancestor* of `feature`, so `feature..
/// main` selects nothing at all, and the annotated tag points at a commit that
/// no such export can reach.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-fx-uninteresting-{tag}-{}", std::process::id()));
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
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/lib.rs"), "pub fn one() -> u32 { 1 }\npub fn two() -> u32 { 2 }\n").unwrap();
    must(&repo, &home, &["add", "src/lib.rs"]);
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

/// The full object id of `rev` in `repo`.
fn oid(repo: &Path, home: &Path, rev: &str) -> String {
    let (stdout, stderr, code) = git(repo, home, &["rev-parse", rev]);
    assert_eq!(code, 0, "rev-parse {rev}: {stderr}");
    stdout.trim().to_string()
}

const NULL_OID: &str = "0000000000000000000000000000000000000000";

/// The reported case: `feature..main` excludes `feature`, `--all` names it again
/// positively, and the export must still be empty.
///
/// `main` is an ancestor of the excluded `feature`, so nothing is exportable —
/// and the second, positive mention of `feature` through `--all` may not revive
/// it. What reaches stdout is only the three `reset`s, in `extra_refs`'
/// name-sorted order walked backwards (`v0.1.0`, `main`, `feature`), each at the
/// null oid because no commit carries a mark. The annotated tag then finds its
/// tagged object unexported and aborts, after those resets are already out.
#[test]
fn a_ref_excluded_by_a_range_is_not_revived_by_all() {
    let (root, repo) = fixture("range-vs-all");
    let home = root.join("home");
    let tag_oid = oid(&repo, &home, "v0.2.0");

    let (stdout, stderr, code) = git(
        &repo,
        &home,
        &["fast-export", "feature..main", "--tag-of-filtered-object=abort", "--all", "--remotes"],
    );

    assert_eq!(
        stdout,
        format!(
            "reset refs/tags/v0.1.0\nfrom {NULL_OID}\n\n\
             reset refs/heads/main\nfrom {NULL_OID}\n\n\
             reset refs/heads/feature\nfrom {NULL_OID}\n\n"
        ),
        "stderr: {stderr}"
    );
    assert_eq!(
        stderr,
        format!(
            "fatal: tag {tag_oid} tags unexported object; \
             use --tag-of-filtered-object=<mode> to handle it\n"
        )
    );
    assert_eq!(code, 128);
}

/// The same exclusion without a tag to abort on: `--tag-of-filtered-object=drop`
/// drops the annotated tag silently, and the three `reset`s are the whole
/// output — a clean exit, so the ordering is not an artifact of the `die()`.
#[test]
fn the_resets_are_the_whole_stream_when_the_tag_is_dropped() {
    let (root, repo) = fixture("range-vs-all-drop");
    let home = root.join("home");

    let (stdout, stderr, code) = git(
        &repo,
        &home,
        &["fast-export", "feature..main", "--tag-of-filtered-object=drop", "--all", "--remotes"],
    );

    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(stderr, "");
    assert_eq!(
        stdout,
        format!(
            "reset refs/tags/v0.1.0\nfrom {NULL_OID}\n\n\
             reset refs/heads/main\nfrom {NULL_OID}\n\n\
             reset refs/heads/feature\nfrom {NULL_OID}\n\n"
        )
    );
}

/// The flag is sticky without `--all` in the picture at all: `main ^main` names
/// one commit twice, once each way, and the negative mention wins outright.
///
/// The positive entry is still recorded, so its ref is still pointed somewhere —
/// at the null oid, since the commit it names was never exported.
#[test]
fn a_commit_named_both_ways_stays_excluded() {
    let (root, repo) = fixture("both-ways");
    let home = root.join("home");

    let (stdout, stderr, code) = git(&repo, &home, &["fast-export", "main", "^main"]);

    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(stderr, "");
    assert_eq!(stdout, format!("reset refs/heads/main\nfrom {NULL_OID}\n\n"));
}

/// The control: exclusion must not become over-exclusion.
///
/// `main..feature` with `--all` excludes only `main`, so the one commit
/// `feature` adds on top of it is still exported, still labelled `refs/heads/
/// feature`, and the refs pointing at the excluded `main` still collapse to the
/// null oid.
#[test]
fn only_the_named_commit_is_excluded() {
    let (root, repo) = fixture("control");
    let home = root.join("home");

    let (stdout, stderr, code) = git(
        &repo,
        &home,
        &["fast-export", "main..feature", "--tag-of-filtered-object=drop", "--all"],
    );

    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        stdout.contains("commit refs/heads/feature\n"),
        "the one reachable commit must still be exported:\n{stdout}"
    );
    assert!(
        stdout.contains(&format!("reset refs/tags/v0.1.0\nfrom {NULL_OID}\n")),
        "a ref on the excluded commit still resets to the null oid:\n{stdout}"
    );
    assert!(
        !stdout.contains("data 8\nadd two"),
        "the excluded commit must not be exported:\n{stdout}"
    );
}
