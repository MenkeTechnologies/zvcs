//! Two divergences that shared a root cause: a refusal that outlived the gap it
//! described, and a whitespace rule that was one case too generous.
//!
//! `pull` refused `-s`/`-X`/`--signoff` on the merge path with "the merge port
//! implements only the 'ort' strategy". That was true once; `merge` has since
//! grown `-s recursive`/`subtree` and the `-X ignore-space-*` rules, so the guard
//! rejected nine argument shapes git accepts. `run_merge()` (builtin/pull.c:541-558)
//! pushes all three verbatim and lets `merge` decide, which is what this now does
//! — leaving one refusal site instead of two.
//!
//! `--ignore-cr-at-eol` stripped a bare trailing CR from an *incomplete* last
//! line. `ends_with_optional_cr()` (xdiff/xutils.c:159-171) computes
//! `complete = s && l[s-1] == '\n'` and only accepts a CR in front of a real
//! terminator — the comment there reads "do not ignore CR at the end of an
//! incomplete line". Ignoring it made `diff --quiet --ignore-cr-at-eol` exit 0
//! where git exits 1, i.e. report a dirty file as clean.
//!
//! Every expectation below was measured against git 2.55.0 before being written.
//! The repositories are built with the zvcs binary itself, so nothing here needs
//! a stock git on PATH and the whole file runs headless.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Identity vars git honors above `user.name`/`user.email`. CI exports these for
/// the whole job, which would rewrite the commits these tests build.
const IDENTITY_ENV: [&str; 6] = [
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
    "GIT_AUTHOR_DATE",
    "GIT_COMMITTER_DATE",
];

fn cmd(dir: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(BIN);
    for var in IDENTITY_ENV {
        c.env_remove(var);
    }
    c.args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "2023-01-01 00:00:00 +0000")
        .env("GIT_COMMITTER_DATE", "2023-01-01 00:00:00 +0000");
    c
}

fn run(dir: &Path, args: &[&str]) {
    let out = cmd(dir, args).output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// (exit code, stdout, stderr) — no success assertion, since several cases here
/// are about a non-zero exit being correct.
fn try_run(dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = cmd(dir, args).output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

fn scratch(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-pullcr-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root.canonicalize().unwrap()
}

/// Upstream and a clone that diverge on the *same line*: upstream re-indents it,
/// the clone edits its text. The indent-only upstream change is what makes
/// `-Xignore-all-space` observably different from a plain merge — with the rule
/// on, the merge is clean; without it, the line conflicts.
fn upstream_and_clone(tag: &str) -> (PathBuf, PathBuf) {
    let root = scratch(tag);
    let up = root.join("up");
    let wt = root.join("wt");

    std::fs::create_dir_all(&up).unwrap();
    run(&root, &["init", "-q", "-b", "main", up.to_str().unwrap()]);
    std::fs::write(up.join("f.txt"), "base\nline2\n").unwrap();
    run(&up, &["add", "f.txt"]);
    run(&up, &["commit", "-q", "-m", "base"]);

    run(&root, &["clone", "-q", up.to_str().unwrap(), wt.to_str().unwrap()]);

    std::fs::write(up.join("f.txt"), "base\n    line2\n").unwrap();
    run(&up, &["commit", "-q", "-am", "upstream reindents"]);

    std::fs::write(wt.join("f.txt"), "base\nline2 local\n").unwrap();
    run(&wt, &["commit", "-q", "-am", "local edits"]);

    (up, wt)
}

/// A one-file repo whose committed content is `before` and worktree content is
/// `after`, both written as raw bytes so CR/newline shapes survive exactly.
fn repo_with_change(tag: &str, before: &[u8], after: &[u8]) -> PathBuf {
    let root = scratch(tag);
    run(&root, &["init", "-q", "-b", "main", "."]);
    std::fs::write(root.join("f.txt"), before).unwrap();
    run(&root, &["add", "f.txt"]);
    run(&root, &["commit", "-q", "-m", "base"]);
    std::fs::write(root.join("f.txt"), after).unwrap();
    root
}

// ---------------------------------------------------------------------------
// pull forwards -s / -X / --signoff to the merge path
// ---------------------------------------------------------------------------

/// The three strategy spellings git routes into `merge_ort_recursive()`
/// (builtin/merge.c:800-801, 833). All must merge, not be refused by pull.
///
/// `-Xours` is present because this fixture diverges on one line: measured on
/// git 2.55.0, all three strategies exit 1 without it and 0 with it, producing a
/// two-parent merge. Carrying the option here also proves `-s` and `-X` are
/// forwarded together, which is the shape `run_merge()` pushes them in.
#[test]
fn pull_forwards_every_strategy_merge_implements() {
    for strategy in ["ort", "recursive", "subtree"] {
        let (_up, wt) = upstream_and_clone(&format!("strat-{strategy}"));
        let (code, _out, err) = try_run(
            &wt,
            &["pull", "--no-rebase", "-s", strategy, "-Xours", "origin", "main"],
        );
        assert_eq!(code, 0, "pull -s {strategy} -Xours should merge; stderr: {err}");
        assert!(
            !err.contains("not supported on the merge path"),
            "pull -s {strategy} still hit the stale guard: {err}"
        );
        // The merge landed as a real merge commit, not a no-op.
        let (_, parents, _) = try_run(&wt, &["rev-list", "--parents", "-1", "HEAD"]);
        assert_eq!(
            parents.split_whitespace().count(),
            3,
            "pull -s {strategy} should have produced a two-parent merge, got: {parents}"
        );
    }
}

/// `-Xignore-all-space` has to reach the merge to matter: upstream's change is
/// indent-only, so with the rule the merge is clean and keeps the local text,
/// and the resulting blob is the local line — not a conflict and not the
/// upstream indentation.
#[test]
fn pull_forwards_strategy_options_that_change_the_result() {
    let (_up, wt) = upstream_and_clone("xopt");
    let (code, _out, err) = try_run(
        &wt,
        &["pull", "--no-rebase", "-Xignore-all-space", "origin", "main"],
    );
    assert_eq!(code, 0, "pull -Xignore-all-space should merge cleanly; stderr: {err}");

    let merged = std::fs::read_to_string(wt.join("f.txt")).unwrap();
    assert!(
        !merged.contains("<<<<<<<"),
        "-Xignore-all-space should have avoided the conflict, got:\n{merged}"
    );
    assert!(
        merged.contains("line2 local"),
        "the local edit should survive the whitespace-insensitive merge, got:\n{merged}"
    );
}

/// Without the rule the same merge conflicts. This is the discriminator: it
/// proves the previous test passes because the option was honored, not because
/// the merge was trivially clean.
#[test]
fn the_same_pull_without_the_option_conflicts() {
    let (_up, wt) = upstream_and_clone("xopt-none");
    let (code, _out, _err) = try_run(&wt, &["pull", "--no-rebase", "origin", "main"]);
    assert_ne!(code, 0, "without -Xignore-all-space this merge must conflict");

    let merged = std::fs::read_to_string(wt.join("f.txt")).unwrap();
    assert!(
        merged.contains("<<<<<<<"),
        "expected conflict markers without the whitespace rule, got:\n{merged}"
    );
}

/// `--signoff` reaches `merge`'s `append_signoff`, so the merge commit carries
/// the trailer; `--no-signoff` must not add one.
#[test]
fn pull_forwards_signoff_both_ways() {
    let (_up, wt) = upstream_and_clone("signoff");
    let (code, _out, err) = try_run(
        &wt,
        &["pull", "--no-rebase", "--signoff", "-Xours", "origin", "main"],
    );
    assert_eq!(code, 0, "pull --signoff should merge; stderr: {err}");
    let (_, msg, _) = try_run(&wt, &["log", "-1", "--format=%B"]);
    assert!(
        msg.contains("Signed-off-by: C O Mitter <committer@example.com>"),
        "--signoff should have added the trailer, message was:\n{msg}"
    );

    let (_up2, wt2) = upstream_and_clone("nosignoff");
    let (code2, _out2, err2) = try_run(
        &wt2,
        &["pull", "--no-rebase", "--no-signoff", "-Xours", "origin", "main"],
    );
    assert_eq!(code2, 0, "pull --no-signoff should merge; stderr: {err2}");
    let (_, msg2, _) = try_run(&wt2, &["log", "-1", "--format=%B"]);
    assert!(
        !msg2.contains("Signed-off-by:"),
        "--no-signoff must not add a trailer, message was:\n{msg2}"
    );
}

/// A strategy `merge` genuinely does not implement must still be refused — and
/// refused by `merge`, so there is exactly one refusal site. The old pull guard
/// would have caught this before `merge` ever saw it.
#[test]
fn a_strategy_merge_does_not_implement_is_refused_by_merge() {
    let (_up, wt) = upstream_and_clone("resolve");
    let (code, _out, err) =
        try_run(&wt, &["pull", "--no-rebase", "-s", "resolve", "origin", "main"]);
    assert_ne!(code, 0, "pull -s resolve must fail");
    assert!(
        !err.contains("not supported on the merge path"),
        "the refusal must come from merge, not pull's old blanket guard: {err}"
    );
    assert!(
        err.contains("resolve"),
        "the refusal should name the strategy: {err}"
    );
}

// ---------------------------------------------------------------------------
// --ignore-cr-at-eol and the incomplete last line
// ---------------------------------------------------------------------------

/// The bug: a final line with no newline that gains a CR is a real change, and
/// `--ignore-cr-at-eol` must not hide it. Measured on git 2.55.0: exit 1.
#[test]
fn cr_gained_on_an_incomplete_last_line_is_not_ignored() {
    let repo = repo_with_change("cr-incomplete", b"x", b"x\r");
    let (code, _out, _err) = try_run(&repo, &["diff", "--quiet", "--ignore-cr-at-eol"]);
    assert_eq!(code, 1, "a CR added to an incomplete last line must still be a difference");
}

/// The mirror: losing a CR from an incomplete last line is equally a change.
#[test]
fn cr_lost_from_an_incomplete_last_line_is_not_ignored() {
    let repo = repo_with_change("cr-incomplete-rev", b"x\r", b"x");
    let (code, _out, _err) = try_run(&repo, &["diff", "--quiet", "--ignore-cr-at-eol"]);
    assert_eq!(code, 1, "a CR removed from an incomplete last line must still be a difference");
}

/// The case the rule exists for, which must keep working: a *complete* line
/// gaining a CR before its newline is ignored. Measured: exit 0.
#[test]
fn cr_before_a_real_newline_is_still_ignored() {
    let repo = repo_with_change("cr-complete", b"x\n", b"x\r\n");
    let (code, _out, _err) = try_run(&repo, &["diff", "--quiet", "--ignore-cr-at-eol"]);
    assert_eq!(code, 0, "a CR before a real newline is exactly what the rule ignores");
}

/// Both shapes in one file: the earlier line's CR is ignorable, the last line's
/// is not, so the file still differs. This is the case a per-line rule that got
/// only the complete-line half right would pass by accident.
#[test]
fn a_file_mixing_both_shapes_still_differs() {
    let repo = repo_with_change("cr-mixed", b"a\nb", b"a\r\nb\r");
    let (code, _out, _err) = try_run(&repo, &["diff", "--quiet", "--ignore-cr-at-eol"]);
    assert_eq!(code, 1, "the incomplete last line's CR keeps the file different");
}

/// The same shape through a counting view rather than the exit code, so the fix
/// is not just an early-exit artifact. Measured on git 2.55.0: `1\t1\tf.txt`.
#[test]
fn numstat_agrees_that_the_incomplete_line_changed() {
    let repo = repo_with_change("cr-numstat", b"x", b"x\r");
    let (code, out, _err) = try_run(&repo, &["diff", "--numstat", "--ignore-cr-at-eol"]);
    assert_eq!(code, 0, "diff --numstat exits 0 even when it reports changes");
    assert_eq!(out.trim(), "1\t1\tf.txt", "expected one line changed");
}

/// `-w` subsumes `--ignore-cr-at-eol` (xdl_recmatch's precedence comment at
/// xdiff/xutils.c:185-188), and CR is whitespace, so this shape *is* clean under
/// `-w`. Guards against "fixing" the CR rule by making all whitespace stricter.
#[test]
fn ignore_all_space_still_swallows_the_same_change() {
    let repo = repo_with_change("cr-w", b"x", b"x\r");
    let (code, _out, _err) = try_run(&repo, &["diff", "--quiet", "--ignore-all-space"]);
    assert_eq!(code, 0, "-w ignores the CR entirely, incomplete line or not");
}
