//! `git fast-export --ancestry-path`.
//!
//! `setup_revisions` turns the option into `revs->ancestry_path` plus
//! `ancestry_path_implicit_bottoms` (`revision.c:2418`), and `limit_list` then
//! runs `collect_bottom_commits` over the pending list and `limit_to_ancestry`
//! over the walked one (`revision.c:1456`/`1502`). Bottom commits are the
//! negative revisions — `handle_revision_arg_1` sets `BOTTOM` on every argument
//! that carries `UNINTERESTING` (`revision.c:2174`) — so a range's left side is
//! one, and an export with no negative revision at all dies before it walks.
//!
//! What survives the filter is only the commits that descend from a bottom
//! commit, which is what drops the other side of a merge. A commit whose first
//! parent goes with it is then exported against the empty tree: `handle_commit`
//! takes the root-diff branch when the first parent carries no mark, so the
//! stanza carries no `from` and lists every path in the tree.
//!
//! Every expectation below was checked against stock git 2.55.0 in the same
//! fixture before being written down.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Run `git <args>` in `repo`, returning stdout, stderr and the exit code.
///
/// Identity and both timestamps are pinned so the fixture — and therefore the
/// whole exported stream, which spells out author and committer lines — is the
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

/// A merge whose two sides are independent: `main commit` on `main` and `side
/// commit` on `side`, both on top of the initial commit, joined by `merge side`.
///
/// `side..main` selects `main commit` and the merge; only the merge descends
/// from the excluded `side`, so `--ancestry-path` has exactly one commit to drop
/// and one to keep.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root =
        std::env::temp_dir().join(format!("zvcs-fx-ancestry-{tag}-{}", std::process::id()));
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
    must(&repo, &home, &["branch", "side"]);
    must(&repo, &home, &["checkout", "-q", "side"]);
    std::fs::write(repo.join("side.txt"), "side work\n").unwrap();
    must(&repo, &home, &["add", "side.txt"]);
    must(&repo, &home, &["commit", "-qm", "side commit"]);
    must(&repo, &home, &["checkout", "-q", "main"]);
    std::fs::write(repo.join("main.txt"), "main work\n").unwrap();
    must(&repo, &home, &["add", "main.txt"]);
    must(&repo, &home, &["commit", "-qm", "main commit"]);
    must(&repo, &home, &["merge", "-q", "--no-ff", "side", "-m", "merge side"]);

    (root, repo)
}

/// The whole point of the option: the side of the merge that does not descend
/// from the bottom commit is gone, and the merge that does is exported as a root.
///
/// `main commit` is neither an ancestor nor a descendant of `side`, so
/// `limit_to_ancestry` marks it UNINTERESTING and it never reaches the stream —
/// not even as a `from`. That leaves the merge's first parent unmarked, so
/// `handle_commit` diffs it against the empty tree and the stanza carries all
/// three paths.
#[test]
fn the_side_that_does_not_descend_from_the_bottom_is_dropped() {
    let (root, repo) = fixture("drops-other-side");
    let home = root.join("home");

    let (stdout, stderr, code) = git(&repo, &home, &["fast-export", "--ancestry-path", "side..main"]);

    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(stderr, "");
    assert_eq!(
        stdout,
        "blob\nmark :1\ndata 10\n# fixture\n\n\
         blob\nmark :2\ndata 10\nmain work\n\n\
         blob\nmark :3\ndata 10\nside work\n\n\
         commit refs/heads/main\nmark :4\n\
         author A U Thor <author@example.com> 1112904793 +0200\n\
         committer C O Mitter <committer@example.com> 1112904793 +0200\n\
         data 11\nmerge side\n\
         M 100644 :1 README.md\nM 100644 :2 main.txt\nM 100644 :3 side.txt\n\n"
    );
}

/// The control: the same range without the option exports both commits, the
/// second one incrementally.
///
/// Without it `main commit` is in the stream and marked, so the merge takes the
/// `from :3` branch and lists only what the merge itself brought in. Any change
/// that made the filter fire unconditionally would show up here.
#[test]
fn the_same_range_without_the_option_keeps_both_commits() {
    let (root, repo) = fixture("control");
    let home = root.join("home");

    let (stdout, stderr, code) = git(&repo, &home, &["fast-export", "side..main"]);

    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(stderr, "");
    assert_eq!(
        stdout,
        "blob\nmark :1\ndata 10\n# fixture\n\n\
         blob\nmark :2\ndata 10\nmain work\n\n\
         commit refs/heads/main\nmark :3\n\
         author A U Thor <author@example.com> 1112904793 +0200\n\
         committer C O Mitter <committer@example.com> 1112904793 +0200\n\
         data 12\nmain commit\n\
         M 100644 :1 README.md\nM 100644 :2 main.txt\n\n\
         blob\nmark :4\ndata 10\nside work\n\n\
         commit refs/heads/main\nmark :5\n\
         author A U Thor <author@example.com> 1112904793 +0200\n\
         committer C O Mitter <committer@example.com> 1112904793 +0200\n\
         data 11\nmerge side\nfrom :3\n\
         M 100644 :4 side.txt\n\n"
    );
}

/// No negative revision means no BOTTOM commit, and `limit_list` dies before it
/// walks anything — so the fatal comes out with an empty stream behind it.
#[test]
fn without_a_negative_revision_it_is_fatal() {
    let (root, repo) = fixture("no-bottom");
    let home = root.join("home");

    let (stdout, stderr, code) = git(&repo, &home, &["fast-export", "--ancestry-path", "main"]);

    assert_eq!(code, 128);
    assert_eq!(stdout, "");
    assert_eq!(
        stderr,
        "fatal: --ancestry-path given but there are no bottom commits\n"
    );
}

/// `--not` marks its revisions BOTTOM as well: `handle_revision_arg_1` derives
/// the flag from UNINTERESTING rather than from the `^` spelling, so the filter
/// behaves identically however the negative side is written.
#[test]
fn a_not_excluded_revision_is_a_bottom_commit_too() {
    let (root, repo) = fixture("not-form");
    let home = root.join("home");

    let (caret, _, caret_code) = git(&repo, &home, &["fast-export", "--ancestry-path", "^side", "main"]);
    let (not, _, not_code) = git(&repo, &home, &["fast-export", "--ancestry-path", "main", "--not", "side"]);

    assert_eq!(caret_code, 0);
    assert_eq!(not_code, 0);
    assert!(caret.contains("data 11\nmerge side\n"), "{caret}");
    assert!(!caret.contains("main commit"), "{caret}");
    assert_eq!(caret, not);
}
