//! Automatic tag following is two-pass, and the split is visible.
//!
//! `do_fetch()` calls `find_non_local_tags()` twice. The first call is inside
//! `get_ref_map()` (builtin/fetch.c:589-591) and runs *before* anything is
//! fetched, so it can only keep a tag whose object is already local or is among
//! the oids the refspecs are about to bring in (:365-385, with `fetch_oids`
//! built by `create_fetch_oidset()` at :250-257). Everything else it drops. The
//! second call is `backfill_tags()` (:2037-2053), which re-examines the dropped
//! tags against the database as it now stands and reports whatever it recovers
//! *after* every other row — including the opportunistic tracking-ref updates,
//! because that call runs its own `store_updated_refs()` pass over a fresh list.
//!
//! Three consequences, all measured against stock git 2.55.0 over a local path:
//!
//! * An annotated tag on the fetched tip is reported before a lightweight tag on
//!   one of its ancestors, even though the remote advertises them the other way
//!   round.
//! * A tag whose object nothing reaches is dropped by both passes, so it gets no
//!   ref, no summary row and no `FETCH_HEAD` line.
//! * `--dry-run` queues nothing (`s_update_ref()` returns early at :651-652), so
//!   the second pass re-proposes the first pass's tags and git prints them twice.
//!
//! One gap remains, stated rather than hidden: git's `--dry-run` still downloads
//! the pack and only skips the ref writes, so its second pass can tell a
//! backfilled tag from one nobody reaches. The vendored fetch downloads nothing
//! under a dry run, so an unreachable tag is still listed there — a real fetch
//! drops it.
//!
//! No network: every remote here is a directory next to the clone.

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

fn err_text(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn ok(dir: &Path, home: &Path, args: &[&str]) -> Output {
    let out = zvcs(dir, home, args);
    assert!(out.status.success(), "{args:?} failed: {}", err_text(&out));
    out
}

/// The position of `needle` in `hay`, or a panic naming what was searched.
fn at(hay: &str, needle: &str) -> usize {
    hay.find(needle).unwrap_or_else(|| panic!("{needle:?} missing from:\n{hay}"))
}

/// A remote whose tags exercise both passes:
///
/// * `aaa-old` — lightweight, on the *first* commit, which is an ancestor of the
///   branch tip. Nothing the refspecs fetch names its object, so the first pass
///   drops it and the backfill round recovers it.
/// * `zzz-tip` — annotated, on the branch tip. Its advertisement carries a `^{}`
///   peel line naming an oid the refspecs are fetching, so the first pass keeps
///   it.
/// * `mmm-orphan` — annotated, on a commit no branch reaches. Both passes drop it.
///
/// The names are chosen so refname order and pass order disagree: the remote
/// advertises `aaa-old`, `mmm-orphan`, `zzz-tip` sorted, while git reports
/// `zzz-tip` (first pass) before `aaa-old` (backfilled). Anything that simply
/// followed the wire order would get it backwards.
fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-tagfollow-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let origin = root.join("origin");
    std::fs::create_dir_all(&home).expect("mkdir home");

    ok(&root, &home, &["init", "-q", "-b", "main", origin.to_str().expect("utf-8")]);
    ok(&origin, &home, &["commit", "--allow-empty", "-q", "-m", "c0"]);
    ok(&origin, &home, &["tag", "aaa-old"]);
    ok(&origin, &home, &["commit", "--allow-empty", "-q", "-m", "c1"]);
    ok(&origin, &home, &["tag", "-a", "zzz-tip", "-m", "zzz-tip"]);
    // A commit on a branch that is then deleted: only `mmm-orphan` names it.
    ok(&origin, &home, &["checkout", "-q", "-b", "gone"]);
    ok(&origin, &home, &["commit", "--allow-empty", "-q", "-m", "unreachable"]);
    ok(&origin, &home, &["tag", "-a", "mmm-orphan", "-m", "mmm-orphan"]);
    ok(&origin, &home, &["checkout", "-q", "main"]);
    ok(&origin, &home, &["branch", "-q", "-D", "gone"]);
    (root, origin, home)
}

/// A fresh empty repository with `origin` pointing at the fixture.
fn consumer(root: &Path, home: &Path, name: &str) -> PathBuf {
    let dir = root.join(name);
    ok(root, home, &["init", "-q", "-b", "main", dir.to_str().expect("utf-8")]);
    ok(&dir, home, &["remote", "add", "origin", "../origin"]);
    dir
}

#[test]
fn backfilled_tags_are_reported_after_the_first_pass() {
    let (root, _origin, home) = fixture("order");
    let work = consumer(&root, &home, "work");

    let err = err_text(&ok(&work, &home, &["fetch", "origin"]));
    let branch = at(&err, "-> origin/main\n");
    let annotated = at(&err, "-> zzz-tip\n");
    let lightweight = at(&err, "-> aaa-old\n");
    assert!(branch < annotated, "the refspec match leads:\n{err}");
    assert!(
        annotated < lightweight,
        "the peeled tag is a first-pass tag and precedes the backfilled one:\n{err}"
    );

    // `FETCH_HEAD` is written inside the same two rounds, so it carries the same
    // order — this is the file `git pull` reads.
    let fetch_head =
        std::fs::read_to_string(work.join(".git/FETCH_HEAD")).expect("read FETCH_HEAD");
    let annotated = at(&fetch_head, "tag 'zzz-tip' of ");
    let lightweight = at(&fetch_head, "tag 'aaa-old' of ");
    assert!(annotated < lightweight, "FETCH_HEAD keeps the pass order:\n{fetch_head}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_unreachable_tag_is_not_followed_at_all() {
    let (root, _origin, home) = fixture("unreachable");
    let work = consumer(&root, &home, "work");

    let err = err_text(&ok(&work, &home, &["fetch", "origin"]));
    assert!(!err.contains("mmm-orphan"), "no summary row for a tag nobody reaches:\n{err}");

    // Not a ref either — the first pass drops it for want of an oid in
    // `fetch_oids`, and the backfill round drops it again because the pack never
    // carried its object.
    let refs = ok(&work, &home, &["for-each-ref", "--format=%(refname)"]);
    let refs = String::from_utf8_lossy(&refs.stdout).into_owned();
    assert!(refs.contains("refs/tags/zzz-tip\n"), "the reachable tags are still there:\n{refs}");
    assert!(refs.contains("refs/tags/aaa-old\n"), "the backfilled tag is still there:\n{refs}");
    assert!(!refs.contains("mmm-orphan"), "no ref for the unreachable tag:\n{refs}");

    // And no `FETCH_HEAD` row: a line here would make `git pull` offer to merge
    // an object the fetch never brought in.
    let fetch_head =
        std::fs::read_to_string(work.join(".git/FETCH_HEAD")).expect("read FETCH_HEAD");
    assert!(!fetch_head.contains("mmm-orphan"), "no FETCH_HEAD row either:\n{fetch_head}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_backfilled_tag_follows_the_opportunistic_update() {
    let (root, _origin, home) = fixture("opportunistic");
    let work = consumer(&root, &home, "work");

    // A command-line refspec with a destination arms tag following *and* leaves
    // the configured refspec to update `refs/remotes/origin/main`
    // opportunistically (builtin/fetch.c:542-543, :593-598). That row is
    // `FETCH_HEAD_IGNORE`, so it sorts last among the first pass — and the
    // backfilled tag still lands behind it, because `backfill_tags()` reports
    // after `fetch_and_consume_refs()` has finished with the whole first list.
    let err = err_text(&ok(&work, &home, &["fetch", "origin", "main:refs/heads/x"]));
    let annotated = at(&err, "-> zzz-tip\n");
    let tracking = at(&err, "-> origin/main\n");
    let lightweight = at(&err, "-> aaa-old\n");
    assert!(annotated < tracking, "the first-pass tag precedes the opportunistic row:\n{err}");
    assert!(tracking < lightweight, "the backfilled tag comes after it:\n{err}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn dry_run_reports_first_pass_tags_twice() {
    let (root, origin, home) = fixture("dry-run");
    // `--dry-run` downloads no pack here, so an unreachable tag cannot be told
    // apart from a backfilled one; it is dropped from the remote to keep this
    // test about the duplication rule alone. See the module docs on the gap.
    ok(&origin, &home, &["tag", "-d", "mmm-orphan"]);
    let work = consumer(&root, &home, "work");

    let err = err_text(&ok(&work, &home, &["fetch", "--dry-run", "origin"]));
    let row = "-> zzz-tip\n";
    assert_eq!(
        err.matches(row).count(),
        2,
        "nothing was queued, so the backfill round proposes zzz-tip again:\n{err}"
    );
    // The backfilled tag was never in the first round, so it stays single.
    assert_eq!(
        err.matches("-> aaa-old\n").count(),
        1,
        "aaa-old belongs to the second round alone:\n{err}"
    );
    // First round: the branch, then the peeled tag. Second round: both tags in
    // refname order, which puts the duplicate zzz-tip last.
    let first_annotated = at(&err, row);
    let lightweight = at(&err, "-> aaa-old\n");
    let second_annotated = err.rfind(row).expect("zzz-tip row");
    assert!(first_annotated < lightweight, "the first round leads:\n{err}");
    assert!(lightweight < second_annotated, "the second round is refname-sorted:\n{err}");

    // A dry run writes nothing at all.
    let refs = ok(&work, &home, &["for-each-ref", "--format=%(refname)"]);
    assert_eq!(String::from_utf8_lossy(&refs.stdout), "", "a dry run creates no refs");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_hung_up_client_leaves_no_diagnostic_from_upload_pack() {
    let (root, _origin, home) = fixture("epipe");
    let work = consumer(&root, &home, "work");

    // The dry run takes the advertisement and hangs up without asking for a
    // pack. Stock `git upload-pack` dies from SIGPIPE and says nothing; the Rust
    // runtime ignores SIGPIPE, so the same event surfaced as EPIPE and printed
    // `fatal: Broken pipe (os error 32)` onto the user's terminal, ahead of the
    // fetch's own summary.
    let err = err_text(&ok(&work, &home, &["fetch", "--dry-run", "origin"]));
    assert!(!err.contains("Broken pipe"), "the server side stays silent:\n{err}");
    assert!(err.starts_with("From ../origin\n"), "the fetch's own header leads:\n{err}");

    let _ = std::fs::remove_dir_all(&root);
}
