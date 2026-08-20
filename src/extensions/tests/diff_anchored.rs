//! `git diff --anchored=<text>` — the anchored patience diff.
//!
//! The option names lines that must appear as *context*: a candidate common line
//! whose text starts with one of the anchor strings can never be dropped in favour
//! of a longer common subsequence, so the diff is forced to run through it. git
//! reaches it only through the patience algorithm, and `--anchored` pins the
//! algorithm to patience for that reason (`diff_opt_anchored()`, diff.c:5544-5556).
//!
//! The fixture is the canonical demonstration: two files that are rotations of one
//! another, where the unanchored diff moves the first block and an anchor on the
//! first line moves the second one instead. Both patches below are bytes captured
//! from stock git 2.55.0.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// `A` and `B` are rotations of each other, so which half moves is entirely the
/// diff engine's choice — which is what makes the anchor observable.
const BEFORE: &str = "a\nb\nc\nd\ne\nf\n";
const AFTER: &str = "c\nd\ne\nf\na\nb\n";

/// What stock git prints with no anchor: the `a`/`b` block moves to the end.
const UNANCHORED_HUNK: &str = "@@ -1,6 +1,6 @@\n-a\n-b\n c\n d\n e\n f\n+a\n+b\n";
/// What stock git prints with `--anchored=a`: `a`/`b` is pinned as context and the
/// `c`-`f` block moves instead.
const ANCHORED_HUNK: &str = "@@ -1,6 +1,6 @@\n+c\n+d\n+e\n+f\n a\n b\n-c\n-d\n-e\n-f\n";

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "2020-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2020-01-01T00:00:00Z")
        .env_remove("GIT_DIR")
        .output()
        .expect("run zvcs git")
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

/// The hunk body of a patch — everything from the `@@` line on — so the assertions
/// do not depend on the blob hashes in the `index` line.
fn hunks(o: &Output) -> String {
    let text = out(o);
    match text.find("@@") {
        Some(at) => text[at..].to_string(),
        None => text,
    }
}

/// A directory with `A` and `B` for the `--no-index` cases.
fn files(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-anchored-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join("A"), BEFORE).expect("write A");
    std::fs::write(dir.join("B"), AFTER).expect("write B");
    dir.canonicalize().expect("canonicalize")
}

/// A repository whose committed `F` is [`BEFORE`] and whose worktree `F` is
/// [`AFTER`], so the in-repo path exercises the same rotation.
fn repo(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-anchored-repo-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    let dir = dir.canonicalize().expect("canonicalize");
    assert!(run(&dir, &["init", "-q", "-b", "main"]).status.success(), "init");
    std::fs::write(dir.join("F"), BEFORE).expect("write");
    assert!(run(&dir, &["add", "F"]).status.success(), "add");
    let o = run(&dir, &["commit", "-q", "-m", "one"]);
    assert!(o.status.success(), "commit: {}", err(&o));
    std::fs::write(dir.join("F"), AFTER).expect("rewrite");
    dir
}

#[test]
fn an_anchor_forces_the_common_subsequence_through_the_named_line() {
    let dir = files("basic");
    assert_eq!(hunks(&run(&dir, &["diff", "--no-index", "A", "B"])), UNANCHORED_HUNK);
    assert_eq!(
        hunks(&run(&dir, &["diff", "--no-index", "--anchored=a", "A", "B"])),
        ANCHORED_HUNK
    );
    // Exit code is git's `--no-index` "files differ" 1 in both cases.
    assert_eq!(code(&run(&dir, &["diff", "--no-index", "--anchored=a", "A", "B"])), 1);
}

#[test]
fn an_anchor_no_line_starts_with_changes_nothing() {
    let dir = files("miss");
    // `is_anchor()` is a prefix test that nothing matches, so `anchor_i` stays -1
    // and the plain patience diff stands.
    assert_eq!(
        hunks(&run(&dir, &["diff", "--no-index", "--anchored=zzz", "A", "B"])),
        hunks(&run(&dir, &["diff", "--no-index", "--patience", "A", "B"]))
    );
    // Anchoring a line that is already common is equally inert here.
    assert_eq!(hunks(&run(&dir, &["diff", "--no-index", "--anchored=c", "A", "B"])), UNANCHORED_HUNK);
}

#[test]
fn the_option_is_repeatable_and_takes_both_spellings() {
    let dir = files("repeat");
    assert_eq!(
        hunks(&run(&dir, &["diff", "--no-index", "--anchored=a", "--anchored=b", "A", "B"])),
        ANCHORED_HUNK
    );
    // The separated form: parse-options takes the next argv entry as the value.
    assert_eq!(hunks(&run(&dir, &["diff", "--no-index", "--anchored", "a", "A", "B"])), ANCHORED_HUNK);
    // An empty anchor is a prefix of every line, so the first candidate anchors.
    assert_eq!(hunks(&run(&dir, &["diff", "--no-index", "--anchored=", "A", "B"])), ANCHORED_HUNK);
}

#[test]
fn patience_clears_the_anchors_and_a_later_anchor_restores_them() {
    let dir = files("order");
    // `diff_opt_patience()` frees every anchor named before it, so the order of the
    // two flags decides the answer.
    assert_eq!(
        hunks(&run(&dir, &["diff", "--no-index", "--anchored=a", "--patience", "A", "B"])),
        UNANCHORED_HUNK
    );
    assert_eq!(
        hunks(&run(&dir, &["diff", "--no-index", "--patience", "--anchored=a", "A", "B"])),
        ANCHORED_HUNK
    );
    // The anchors survive a later algorithm change but are inert under it, because
    // only `xdl_do_patience_diff()` reads them.
    assert_eq!(
        hunks(&run(&dir, &["diff", "--no-index", "--anchored=a", "--histogram", "A", "B"])),
        hunks(&run(&dir, &["diff", "--no-index", "--histogram", "A", "B"]))
    );
    // …and a *second* `--anchored` after it re-pins the algorithm to patience.
    assert_eq!(
        hunks(&run(&dir, &["diff", "--no-index", "--anchored=a", "--histogram", "--anchored=b", "A", "B"])),
        ANCHORED_HUNK
    );
}

#[test]
fn anchoring_works_against_the_index_and_a_tree() {
    let dir = repo("tree");
    assert_eq!(hunks(&run(&dir, &["diff", "F"])), UNANCHORED_HUNK);
    assert_eq!(hunks(&run(&dir, &["diff", "--anchored=a", "F"])), ANCHORED_HUNK);
    assert_eq!(hunks(&run(&dir, &["diff", "--anchored=a", "HEAD", "--", "F"])), ANCHORED_HUNK);
    assert!(run(&dir, &["add", "F"]).status.success());
    assert_eq!(hunks(&run(&dir, &["diff", "--anchored=a", "--cached", "F"])), ANCHORED_HUNK);
    // `git log -p` takes the same diff option through `setup_revisions()`.
    let o = run(&dir, &["commit", "-q", "-m", "two"]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&run(&dir, &["log", "-p", "--anchored=a", "-1"])).contains(ANCHORED_HUNK));
    assert!(out(&run(&dir, &["log", "-p", "-1"])).contains(UNANCHORED_HUNK));
}

#[test]
fn the_anchored_diff_feeds_the_other_output_formats() {
    let dir = files("formats");
    // The stat block counts the anchored diff's four-line move, not the two-line one.
    let o = run(&dir, &["diff", "--no-index", "--anchored=a", "--stat", "A", "B"]);
    assert!(out(&o).contains("4 insertions(+), 4 deletions(-)"), "{}", out(&o));
    let o = run(&dir, &["diff", "--no-index", "--stat", "A", "B"]);
    assert!(out(&o).contains("2 insertions(+), 2 deletions(-)"), "{}", out(&o));

    // `-U0` splits the same change into two hunks.
    let o = run(&dir, &["diff", "--no-index", "--anchored=a", "-U0", "A", "B"]);
    assert_eq!(hunks(&o), "@@ -0,0 +1,4 @@\n+c\n+d\n+e\n+f\n@@ -3,4 +6,0 @@ b\n-c\n-d\n-e\n-f\n");
}

#[test]
fn a_missing_anchor_value_is_a_usage_error() {
    let dir = repo("noval");
    // `get_arg()`'s `error(_("%s requires a value"))`, then `PARSE_OPT_ERROR`'s 129.
    //
    // Only the case where the flag is the last *option* is asserted: a value-taking
    // option written after a pathspec is `setup_revisions()`'s `fatal: option '%s'
    // must come before non-option arguments` at 128, and this port answers every one
    // of those — `--diff-algorithm`, `-S`, `--find-object`, `--color-moved-ws` and
    // now `--anchored` alike — with the missing-value wording instead. That gap is
    // older than this option and is deliberately not pinned here.
    let o = run(&dir, &["diff", "--anchored"]);
    assert_eq!(code(&o), 129);
    assert_eq!(err(&o), "error: option `anchored' requires a value\n");

    let dir = files("noval-noindex");
    let o = run(&dir, &["diff", "--no-index", "A", "B", "--anchored"]);
    assert_eq!(code(&o), 129);
    assert_eq!(err(&o), "error: option `anchored' requires a value\n");
}
