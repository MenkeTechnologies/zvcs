//! `handle_revision_arg_1()`'s parent marks — `<rev>^@`, `<rev>^!`, `<rev>^-<n>`
//! — for the four verbs that read revisions out of argv without going through
//! `blame`'s copy of the block: `format-patch`, `bundle create`, `fast-export`
//! and `reflog show`.
//!
//! The marks are `handle_revision_arg_1()`'s own grammar, not the revision
//! parser's: `get_oid_1()` has no case for any of them, and `setup_revisions()`
//! strips them *before* the operand is ever resolved. A command that skips the
//! block therefore cannot mis-name the operand — it cannot resolve it at all,
//! and `git format-patch --stdout HEAD^!` becomes
//! `fatal: ambiguous argument 'HEAD^!'` where stock emits one patch.
//!
//! What the block decides, and what each assertion below pins down:
//!
//! * `^@` **replaces** the operand. `add_parents_only()` queues the parents
//!   under `flags` and `handle_revision_arg_1()` returns, so the named commit is
//!   never pended.
//! * `^!` and `^-<n>` only **prepend**. The parents go in under
//!   `flags ^ (UNINTERESTING | BOTTOM)` and the truncated name carries on to the
//!   single-name path, which is what makes `<rev>^!` the range `<rev>^..<rev>`.
//! * A mark `add_parents_only()` declines — a parent number past the commit's
//!   parent count, a name that is not commit-ish — leaves the operand alone, so
//!   it reaches the resolver still carrying its mark and fails there.
//! * A parent number below one (`^-0`) and a second mark behind the first
//!   (`main^!^!`, which `strstr` will not strip) are refused outright.
//! * The mark decides how many times the operand is **resolved**, and therefore
//!   how many `warning: refname … is ambiguous.` lines it earns: two for `^!`
//!   and `^-<n>`, one for `^@`.
//!
//! Every expectation here was measured against stock git 2.55.0 in this exact
//! fixture before being written down.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Run `git <args>` in `repo`. Identity and both timestamps are pinned so the
/// fixture's object ids are reproducible.
fn git(repo: &Path, home: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(repo)
        .env("ZVCS_HOME", home)
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "2005-04-07T22:13:13 +0200")
        .env("GIT_COMMITTER_DATE", "2005-04-07T22:13:13 +0200")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
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

fn rev(repo: &Path, home: &Path, spec: &str) -> String {
    let (out, stderr, code) = git(repo, home, &["rev-parse", spec]);
    assert_eq!(code, 0, "rev-parse {spec} failed: {stderr}");
    out.trim().to_owned()
}

/// A root commit, a two-commit `main`, a `side` branch off the root's child, a
/// merge of the two, one more commit on top, and an annotated tag on the merge.
///
/// The merge is what gives `^-<n>` more than one parent to choose between, and
/// the annotated tag is what makes `add_parents_only()`'s tag-peeling loop
/// observable.
///
/// `ambiguous` additionally creates a branch whose *name* is the merge's
/// 40-hex id, which is the one shape both stock and this port implement the
/// `warning: refname … is ambiguous.` for — so the warning counts below are
/// measurable rather than assumed.
fn fixture(tag: &str, ambiguous: bool) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir()
        .join(format!("zvcs-parent-marks-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    let home = root.join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let repo = repo.canonicalize().unwrap();
    let home = home.canonicalize().unwrap();

    must(&repo, &home, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("f"), "one\n").unwrap();
    must(&repo, &home, &["add", "f"]);
    must(&repo, &home, &["commit", "-qm", "one"]);
    std::fs::write(repo.join("f"), "two\n").unwrap();
    must(&repo, &home, &["add", "f"]);
    must(&repo, &home, &["commit", "-qm", "two"]);
    must(&repo, &home, &["checkout", "-q", "-b", "side", "HEAD~1"]);
    std::fs::write(repo.join("g"), "side\n").unwrap();
    must(&repo, &home, &["add", "g"]);
    must(&repo, &home, &["commit", "-qm", "side"]);
    must(&repo, &home, &["checkout", "-q", "main"]);
    must(&repo, &home, &["merge", "-q", "--no-ff", "-m", "merge", "side"]);
    std::fs::write(repo.join("f"), "three\n").unwrap();
    must(&repo, &home, &["add", "f"]);
    must(&repo, &home, &["commit", "-qm", "three"]);
    must(&repo, &home, &["tag", "-a", "-m", "tagmsg", "v1", "HEAD~1"]);
    if ambiguous {
        let merge = rev(&repo, &home, "HEAD~1");
        must(&repo, &home, &["update-ref", &format!("refs/heads/{merge}"), "HEAD~2"]);
    }
    (repo, home)
}

/// How many patches a `format-patch --stdout` run emitted.
fn patches(stdout: &str) -> usize {
    stdout.lines().filter(|l| l.starts_with("From ") && l.ends_with(" Mon Sep 17 00:00:00 2001")).count()
}

/// `warning: refname … is ambiguous.` lines, ignoring the advice paragraph the
/// message drags along.
fn ambiguity_warnings(stderr: &str) -> usize {
    stderr.lines().filter(|l| l.starts_with("warning: refname ") && l.ends_with(" is ambiguous.")).count()
}

const AMBIGUOUS_ARGUMENT: &str = "unknown revision or path not in the working tree";

/// `^!` is `<rev>^..<rev>`: the commit itself, with every parent excluded.
///
/// The pending list ends up two objects long, so `cmd_format_patch`'s `<since>`
/// shorthand does not fire and the walk is the range as written.
#[test]
fn format_patch_bang_mark_is_the_commit_alone() {
    let (repo, home) = fixture("fp-bang", false);
    let (out, err, code) = git(&repo, &home, &["format-patch", "--stdout", "HEAD^!"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(patches(&out), 1, "{out}");
    assert!(out.contains("Subject: [PATCH] three"), "{out}");
    assert!(err.is_empty(), "{err}");
}

/// `^@` replaces the operand with its parents, so `HEAD^@` pends exactly one
/// object — and *that* is what makes `cmd_format_patch`'s traditional
/// `<since>` shorthand fire, turning the operand into `HEAD^..HEAD`.
///
/// The output is therefore the same single patch as `HEAD^!` by a completely
/// different route, which is the detail a port is most likely to get wrong by
/// pending the commit as well.
#[test]
fn format_patch_at_mark_pends_only_the_parents() {
    let (repo, home) = fixture("fp-at", false);
    let (out, err, code) = git(&repo, &home, &["format-patch", "--stdout", "HEAD^@"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(patches(&out), 1, "{out}");
    assert!(out.contains("Subject: [PATCH] three"), "{out}");
}

/// `^-<n>` excludes only the `n`th parent, so a merge's `^-2` shows the first
/// parent's side of the history and nothing of the second's.
#[test]
fn format_patch_dash_mark_selects_one_parent() {
    let (repo, home) = fixture("fp-dash", false);
    let (out, err, code) = git(&repo, &home, &["format-patch", "--stdout", "HEAD~1^-2"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(patches(&out), 1, "{out}");
    assert!(out.contains("Subject: [PATCH] two"), "{out}");
}

/// The three ways a mark stops being a mark, all of which end at the same
/// `ambiguous argument` because the operand reaches the resolver intact:
///
/// * `^-3` on a two-parent merge — `add_parents_only()` returns 0 for a parent
///   number past the count, which is *not* an error;
/// * `^-0` — `exclude_parent < 1` is refused before `add_parents_only()` runs;
/// * `main^!^!` — `strstr()` finds the *first* `^!` and `!mark[2]` then rejects
///   it, so nothing is stripped.
#[test]
fn format_patch_declined_marks_fall_through_to_the_resolver() {
    let (repo, home) = fixture("fp-declined", false);
    for spec in ["HEAD~1^-3", "main^-0", "main^--1", "main^!^!", "main^@^@"] {
        let (out, err, code) = git(&repo, &home, &["format-patch", "--stdout", spec]);
        assert_eq!(code, 128, "{spec}: {err}");
        assert!(out.is_empty(), "{spec}: {out}");
        assert!(
            err.starts_with(&format!("fatal: ambiguous argument '{spec}': {AMBIGUOUS_ARGUMENT}")),
            "{spec}: {err}"
        );
    }
}

/// `add_parents_only()`'s `get_reference()` dies naming the **base**, not the
/// operand: the mark is already off by the time `parse_object()` is reached, so
/// an absent full-length hex is reported with fourteen fewer characters than
/// were typed.
#[test]
fn absent_full_hex_with_a_mark_is_bad_object_naming_the_base() {
    let (repo, home) = fixture("bad-object", false);
    let zeros = "0".repeat(40);
    for mark in ["^!", "^@", "^-1"] {
        let spec = format!("{zeros}{mark}");
        let (_, err, code) = git(&repo, &home, &["format-patch", "--stdout", &spec]);
        assert_eq!(code, 128, "{spec}: {err}");
        assert_eq!(err, format!("fatal: bad object {zeros}\n"), "{spec}");
    }
}

/// The operand is resolved once per mark and once more on the fall-through, so
/// the ambiguity warning is printed twice for `^!` and `^-<n>` and once for
/// `^@`. Over-warning is the failure mode a port lands in by resolving the base
/// again for its own bookkeeping.
///
/// The merge's `^!` also happens to print nothing: `format-patch` sets
/// `rev.max_parents = 1`, so the one commit the range selects is dropped.
#[test]
fn mark_resolutions_are_counted_the_way_git_counts_them() {
    let (repo, home) = fixture("warnings", true);
    let merge = rev(&repo, &home, "HEAD~1");

    let (out, err, code) = git(&repo, &home, &["format-patch", "--stdout", &format!("{merge}^!")]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(patches(&out), 0, "{out}");
    assert_eq!(ambiguity_warnings(&err), 2, "{err}");

    let (out, err, code) = git(&repo, &home, &["format-patch", "--stdout", &format!("{merge}^-1")]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(ambiguity_warnings(&err), 2, "{err}");
    assert_eq!(patches(&out), 1, "{out}");

    // `^@` returns from `handle_revision_arg_1()` before the second resolution.
    let (out, err, code) = git(&repo, &home, &["format-patch", "--stdout", &format!("{merge}^@")]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(ambiguity_warnings(&err), 1, "{err}");
    assert_eq!(patches(&out), 3, "{out}");
}

/// `git bundle create` writes the header from `revs->pending`, so `HEAD^!`
/// contributes both the tip it keeps and the parent it excludes: one ref line
/// and one prerequisite.
#[test]
fn bundle_bang_mark_becomes_a_prerequisite() {
    let (repo, home) = fixture("bundle-bang", false);
    let head = rev(&repo, &home, "HEAD");
    let parent = rev(&repo, &home, "HEAD^");
    let path = repo.join("out.bundle");
    let file = path.to_str().unwrap();

    let (_, err, code) = git(&repo, &home, &["bundle", "create", file, "HEAD^!"]);
    assert_eq!(code, 0, "{err}");

    let (out, err, code) = git(&repo, &home, &["bundle", "list-heads", file]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out, format!("{head} HEAD\n"), "{out}");

    // The bundle is not self-contained: the excluded parent has to be present
    // already, which is exactly what `bundle verify` reports.
    let (out, err, code) = git(&repo, &home, &["bundle", "verify", file]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("The bundle requires this ref:"), "{out}");
    assert!(out.contains(&parent), "{out}");
}

/// `add_pending_object(revs, it, arg)` names a queued parent by the **base**, so
/// `git bundle create <file> HEAD^@` writes the parent's id under the name
/// `HEAD` — a header line whose id is deliberately not what `HEAD` resolves to.
#[test]
fn bundle_at_mark_names_the_parent_after_the_base() {
    let (repo, home) = fixture("bundle-at", false);
    let parent = rev(&repo, &home, "HEAD^");
    let path = repo.join("out.bundle");
    let file = path.to_str().unwrap();

    let (_, err, code) = git(&repo, &home, &["bundle", "create", file, "HEAD^@"]);
    assert_eq!(code, 0, "{err}");
    let (out, err, code) = git(&repo, &home, &["bundle", "list-heads", file]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out, format!("{parent} HEAD\n"), "{out}");
}

/// `setup_revisions()` keeps the operand **as typed** in `arg_` and only moves
/// `arg`, so `add_rev_cmdline()` records `main^!` while `add_pending_object()`
/// records `main`. `main^!` does not dwim to a ref, which is why the commit is
/// labelled `commit main` and not `commit refs/heads/main`.
#[test]
fn fast_export_labels_a_bang_mark_by_the_pending_name() {
    let (repo, home) = fixture("fe-bang", false);
    let (out, err, code) = git(&repo, &home, &["fast-export", "main^!"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        out,
        "blob\n\
         mark :1\n\
         data 6\n\
         three\n\
         \n\
         blob\n\
         mark :2\n\
         data 5\n\
         side\n\
         \n\
         commit main\n\
         mark :3\n\
         author A U Thor <author@example.com> 1112904793 +0200\n\
         committer C O Mitter <committer@example.com> 1112904793 +0200\n\
         data 6\n\
         three\n\
         M 100644 :1 f\n\
         M 100644 :2 g\n\
         \n",
        "{out}"
    );
}

/// `get_tags_and_duplicates()` branches on `e->item->type` — the object the
/// *cmdline entry* holds. For `v1^@` that object is the tag's commit's parent,
/// so the entry is an ordinary commit ref and the annotated tag is never
/// exported; reading the dwim'd ref's own target instead turns this into
/// `fatal: tag … tags unexported object`.
#[test]
fn fast_export_at_mark_on_a_tag_exports_no_tag_object() {
    let (repo, home) = fixture("fe-at-tag", false);
    let (out, err, code) = git(&repo, &home, &["fast-export", "v1^@"]);
    assert_eq!(code, 0, "{err}");
    assert!(!out.contains("\ntag "), "an annotated tag was exported: {out}");
    assert_eq!(out.matches("commit refs/tags/v1\n").count(), 3, "{out}");
    assert_eq!(out.matches("\nreset refs/tags/v1\n").count(), 1, "{out}");
    // The merge's own commit is not among them: `^@` selects the parents.
    assert!(!out.contains("data 5\nmerge\n"), "{out}");
}

/// `add_reflog_for_walk()` opens with
/// `if (commit->object.flags & UNINTERESTING) die("cannot walk reflogs for %s", name)`,
/// and `^!` queues every parent UNINTERESTING — so `git reflog show HEAD^!` is a
/// fatal naming `HEAD`, the base, rather than the operand.
///
/// The same refusal is what a bare `^<commit>` earns, which is the other half of
/// the flag this block sets.
#[test]
fn reflog_refuses_an_uninteresting_commit() {
    let (repo, home) = fixture("reflog-bang", false);
    for (spec, name) in [("HEAD^!", "HEAD"), ("HEAD^-1", "HEAD"), ("^main", "main"), ("v1^!", "v1")] {
        let (out, err, code) = git(&repo, &home, &["reflog", "show", spec]);
        assert_eq!(code, 128, "{spec}: {err}");
        assert!(out.is_empty(), "{spec}: {out}");
        assert_eq!(err, format!("fatal: cannot walk reflogs for {name}\n"), "{spec}");
    }
}

/// `^@` keeps `flags`, so the parents are queued *interesting* and
/// `add_reflog_for_walk()` reads the log of the name they were queued under —
/// the base. `git reflog show HEAD^@` is therefore `git reflog show HEAD`, and
/// `git reflog show <no-such-log>^@` is silent rather than fatal.
#[test]
fn reflog_at_mark_walks_the_base_name() {
    let (repo, home) = fixture("reflog-at", false);
    let (plain, err, code) = git(&repo, &home, &["reflog", "show", "HEAD"]);
    assert_eq!(code, 0, "{err}");
    assert!(plain.lines().count() > 1, "{plain}");

    let (marked, err, code) = git(&repo, &home, &["reflog", "show", "HEAD^@"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(marked, plain, "HEAD^@ did not walk HEAD's reflog");

    // `v1` owns no reflog, so its parents' walk finds nothing and prints
    // nothing — `add_reflog_for_walk()` returning -1 is not an error.
    let (out, err, code) = git(&repo, &home, &["reflog", "show", "v1^@"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.is_empty(), "{out}");
}

/// The marks are `handle_revision_arg_1()`'s grammar, so every verb that reaches
/// `setup_revisions()` accepts them — and a declined one is a plain
/// `ambiguous argument` in each, never a silent empty result. Before this block
/// existed, `format-patch --stdout HEAD^!` emitted zero patches and
/// `fast-export HEAD^!` exported the wrong set, both with exit 0.
#[test]
fn every_verb_reads_the_same_mark_grammar() {
    let (repo, home) = fixture("all-verbs", false);
    let path = repo.join("out.bundle");
    let file = path.to_str().unwrap();
    let bundle = ["bundle", "create", file, "HEAD^!"];
    for args in [
        &["format-patch", "--stdout", "HEAD^!"][..],
        &bundle[..],
        &["fast-export", "HEAD^!"][..],
    ] {
        let (_, err, code) = git(&repo, &home, args);
        assert_eq!(code, 0, "{args:?}: {err}");
        assert!(err.is_empty(), "{args:?}: {err}");
    }
    let declined = ["bundle", "create", file, "HEAD^-9"];
    for args in [
        &["format-patch", "--stdout", "HEAD^-9"][..],
        &declined[..],
        &["fast-export", "HEAD^-9"][..],
        &["reflog", "show", "HEAD^-9"][..],
    ] {
        let (_, err, code) = git(&repo, &home, args);
        assert_eq!(code, 128, "{args:?}: {err}");
        assert!(err.contains(AMBIGUOUS_ARGUMENT), "{args:?}: {err}");
    }
}
