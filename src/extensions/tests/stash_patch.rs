//! `git stash -p` — the hunk selector against a scratch index.
//!
//! The invariants here are the ones that make patch mode different from a plain
//! push, and each of them was verified against stock git 2.55.0 on the same
//! fixture: only the selected hunks reach the stash, every *unselected* edit stays
//! on disk (a tree-level reset cannot do this — two hunks of one file land on
//! opposite sides of the selection), the real index is untouched because the
//! selector stages into `$GIT_DIR/index.stash.<pid>` instead, that scratch index
//! is gone afterwards, and an empty selection stores nothing at all.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");
const DATE: &str = "1136214245 +0000";

fn git(dir: &Path, args: &[&str]) -> Output {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_DATE", DATE)
        .env("GIT_COMMITTER_DATE", DATE)
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .output()
        .unwrap();
    out
}

fn git_ok(dir: &Path, args: &[&str]) -> String {
    let out = git(dir, args);
    assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Run a command that reads the hunk selector's answers from stdin.
fn git_input(dir: &Path, args: &[&str], input: &str) -> Output {
    use std::io::Write;
    let mut child = Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_DATE", DATE)
        .env("GIT_COMMITTER_DATE", DATE)
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

/// A repo whose worktree holds two separated edits in `f1` (so the file has two
/// hunks), one edit in `f2`, and a staged new file `f3`.
fn fixture(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-stashp-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let repo = root.canonicalize().unwrap();
    git_ok(&repo, &["init", "-q", "-b", "main"]);
    git_ok(&repo, &["config", "user.email", "author@example.com"]);
    git_ok(&repo, &["config", "user.name", "A U Thor"]);
    std::fs::write(repo.join("f1"), "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n").unwrap();
    std::fs::write(repo.join("f2"), "one\ntwo\nthree\n").unwrap();
    git_ok(&repo, &["add", "f1", "f2"]);
    git_ok(&repo, &["commit", "-q", "-m", "base"]);
    std::fs::write(repo.join("f1"), "a-CHANGED\nb\nc\nd\ne\nf\ng\nh\ni\nj-CHANGED\n").unwrap();
    std::fs::write(repo.join("f2"), "one\ntwo-CHANGED\nthree\n").unwrap();
    std::fs::write(repo.join("f3"), "x\n").unwrap();
    git_ok(&repo, &["add", "f3"]);
    repo
}

/// No `index.stash.<pid>` may survive the run: git removes the scratch index on
/// entry and again at `done:`, and a leaked one would be picked up by the next
/// process with the same pid.
fn no_scratch_index(repo: &Path) {
    let leaked: Vec<String> = std::fs::read_dir(repo.join(".git"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("index.stash."))
        .collect();
    assert!(leaked.is_empty(), "scratch index left behind: {leaked:?}");
}

#[test]
fn selected_hunk_is_stashed_and_the_rest_stays_on_disk() {
    let repo = fixture("take-one");
    // Take f1's first hunk, refuse its second, refuse f2's only hunk.
    let out = git_input(&repo, &["stash", "-p"], "y\nn\nn\n");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Saved working directory and index state WIP on main:"),
        "stdout: {stdout}"
    );

    // The stash holds exactly the hunk that was taken.
    let show = git_ok(&repo, &["stash", "show", "-p"]);
    assert!(show.contains("+a-CHANGED"), "stash lost the selected hunk: {show}");
    assert!(!show.contains("j-CHANGED"), "stash took an unselected hunk: {show}");
    assert!(!show.contains("f2"), "stash took an unselected file: {show}");

    // The worktree lost that hunk and kept every other edit.
    let f1 = std::fs::read_to_string(repo.join("f1")).unwrap();
    assert!(f1.starts_with("a\n"), "selected hunk was not reversed: {f1:?}");
    assert!(f1.ends_with("j-CHANGED\n"), "unselected hunk was destroyed: {f1:?}");
    assert_eq!(std::fs::read_to_string(repo.join("f2")).unwrap(), "one\ntwo-CHANGED\nthree\n");

    // `--patch` implies `--keep-index`, so the staged file is still staged.
    let staged = git_ok(&repo, &["diff", "--cached", "--name-only"]);
    assert_eq!(staged, "f3\n", "patch mode disturbed the index");
    no_scratch_index(&repo);
}

#[test]
fn selecting_nothing_stores_nothing() {
    let repo = fixture("take-none");
    let out = git_input(&repo, &["stash", "-p"], "n\nn\nn\n");
    assert_eq!(out.status.code(), Some(1), "stock exits 1 when nothing was selected");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("No changes selected"), "stderr: {stderr}");

    assert_eq!(git_ok(&repo, &["stash", "list"]), "", "a stash was stored anyway");
    // Every edit is still on disk.
    let f1 = std::fs::read_to_string(repo.join("f1")).unwrap();
    assert!(f1.starts_with("a-CHANGED\n") && f1.ends_with("j-CHANGED\n"), "{f1:?}");
    assert_eq!(std::fs::read_to_string(repo.join("f2")).unwrap(), "one\ntwo-CHANGED\nthree\n");
    no_scratch_index(&repo);
}

#[test]
fn quitting_the_selector_stores_nothing() {
    let repo = fixture("quit");
    let out = git_input(&repo, &["stash", "-p"], "q\n");
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("No changes selected"));
    assert_eq!(git_ok(&repo, &["stash", "list"]), "");
    no_scratch_index(&repo);
}

#[test]
fn no_keep_index_refreshes_the_index_against_the_rewound_worktree() {
    let repo = fixture("no-keep");
    let out = git_input(&repo, &["stash", "-p", "--no-keep-index"], "y\nn\nn\n");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    // `keep_index < 1` runs `reset -q --refresh --`, whose pathspec-less form is
    // a mixed reset: the index goes back to HEAD, so the file that was staged is
    // untracked afterwards (verified against stock git 2.55.0, which leaves the
    // same `?? f3`). Its content is still on disk — only the staging is undone.
    let staged = git_ok(&repo, &["diff", "--cached", "--name-only"]);
    assert_eq!(staged, "", "the index was not reset");
    assert_eq!(std::fs::read_to_string(repo.join("f3")).unwrap(), "x\n");
    let f1 = std::fs::read_to_string(repo.join("f1")).unwrap();
    assert!(f1.starts_with("a\n") && f1.ends_with("j-CHANGED\n"), "{f1:?}");
    no_scratch_index(&repo);
}

#[test]
fn patch_is_refused_with_untracked_and_required_by_the_selector_options() {
    let repo = fixture("refusals");
    // `do_push_stash()`: the two describe different things to capture.
    let out = git(&repo, &["stash", "-p", "-u"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr).trim_end(),
        "Can't use --patch and --include-untracked or --all at the same time"
    );
    // The selector's knobs are inert without `--patch`, and git refuses them.
    let out = git(&repo, &["stash", "-U2"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr).trim_end(),
        "fatal: the option '--unified' requires '--patch'"
    );
    // `push` checks `requires` before `cannot be negative`; `save` checks them
    // the other way round, so both orders are pinned.
    let out = git(&repo, &["stash", "-p", "-U-5"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr).trim_end(),
        "fatal: '--unified' cannot be negative"
    );
    let out = git(&repo, &["stash", "save", "-U-5"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr).trim_end(),
        "fatal: '--unified' cannot be negative"
    );
    // Nothing above may have stashed anything.
    assert_eq!(git_ok(&repo, &["stash", "list"]), "");
    no_scratch_index(&repo);
}

#[test]
fn a_pathspec_limits_the_selector() {
    let repo = fixture("pathspec");
    let out = git_input(&repo, &["stash", "-p", "--", "f2"], "y\n");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let show = git_ok(&repo, &["stash", "show", "-p"]);
    assert!(show.contains("+two-CHANGED"), "{show}");
    assert!(!show.contains("f1"), "the pathspec did not limit the selection: {show}");
    // f1's edits are untouched; f2's is gone from the worktree.
    let f1 = std::fs::read_to_string(repo.join("f1")).unwrap();
    assert!(f1.starts_with("a-CHANGED\n") && f1.ends_with("j-CHANGED\n"), "{f1:?}");
    assert_eq!(std::fs::read_to_string(repo.join("f2")).unwrap(), "one\ntwo\nthree\n");
    no_scratch_index(&repo);
}
