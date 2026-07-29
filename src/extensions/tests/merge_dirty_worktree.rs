//! `git merge` refuses per path, not per worktree.
//!
//! git runs two different gates and this port used to collapse them into a
//! single "is anything dirty" test, which refused merges git performs — most
//! visibly `git pull` into a tree carrying local edits the pull does not touch.
//! The gates, as `merge-ort.c` and `unpack_trees.c` implement them:
//!
//! * merge-ort's `merge_start()` refuses when the *index* differs from `HEAD`
//!   anywhere. Only a strategy runs it, so a fast-forward accepts staged work.
//! * `twoway_merge()` + `verify_uptodate()`/`verify_absent()` refuse per path,
//!   and only for paths the two trees disagree on.
//!
//! Every expectation below was taken from stock git 2.50.1 byte for byte,
//! including which stream each line lands on and the exit code.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"))
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = run(dir, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn write(dir: &Path, path: &str, body: &str) {
    std::fs::write(dir.join(path), body).unwrap();
}

fn read(dir: &Path, path: &str) -> String {
    std::fs::read_to_string(dir.join(path)).unwrap()
}

fn temp_root(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-mergedirty-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root.canonicalize().unwrap()
}

/// `main` holding `a`, `b`, `c`, plus a `feat` branch one commit ahead of it
/// that rewrites `a` and adds `n` — so `main` fast-forwards onto `feat`, and the
/// merge's footprint is exactly `a` and `n`.
fn fast_forwardable(tag: &str) -> PathBuf {
    let root = temp_root(tag);
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main", "."]);
    git(&repo, &["config", "user.email", "t@example.com"]);
    git(&repo, &["config", "user.name", "T"]);
    for f in ["a", "b", "c"] {
        write(&repo, f, "v1\n");
    }
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "base"]);

    git(&repo, &["checkout", "-q", "-b", "feat"]);
    write(&repo, "a", "v2\n");
    write(&repo, "n", "new\n");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "feat"]);
    git(&repo, &["checkout", "-q", "main"]);
    repo
}

/// [`fast_forwardable`] with `main` moved on too, so `feat` is a genuine
/// three-way merge rather than a fast-forward.
fn diverged(tag: &str) -> PathBuf {
    let repo = fast_forwardable(tag);
    write(&repo, "c", "moved\n");
    git(&repo, &["commit", "-q", "-am", "main moves"]);
    repo
}

/// The reported bug: a fast-forward with local edits outside its footprint is a
/// merge git performs, and the edits have to still be there afterwards.
#[test]
fn a_pull_fast_forwards_over_unrelated_local_changes() {
    let root = temp_root("pull");
    let upstream = root.join("up");
    std::fs::create_dir_all(&upstream).unwrap();
    git(&upstream, &["init", "-q", "-b", "main", "."]);
    git(&upstream, &["config", "user.email", "t@example.com"]);
    git(&upstream, &["config", "user.name", "T"]);
    write(&upstream, "a", "v1\n");
    write(&upstream, "b", "v1\n");
    git(&upstream, &["add", "-A"]);
    git(&upstream, &["commit", "-q", "-m", "base"]);

    let clone = root.join("clone");
    git(&root, &["clone", "-q", "up", "clone"]);

    write(&upstream, "a", "v2\n");
    git(&upstream, &["commit", "-q", "-am", "upstream moves a"]);

    // A local edit to a file the pull does not carry.
    write(&clone, "b", "local work\n");

    let out = run(&clone, &["pull"]);
    assert!(
        out.status.success(),
        "pull refused a fast-forward it could perform: {}{}",
        stdout(&out),
        stderr(&out)
    );
    assert_eq!(read(&clone, "a"), "v2\n", "the pull did not land");
    assert_eq!(
        read(&clone, "b"),
        "local work\n",
        "the pull clobbered a local change outside its footprint"
    );
    assert_eq!(
        git(&clone, &["status", "--porcelain"]),
        " M b\n",
        "the local change stopped being reported as a modification"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// The other half of the same gate: a path the fast-forward *does* rewrite is
/// refused, with git's wording, on git's streams, at git's exit code.
#[test]
fn a_fast_forward_refuses_the_paths_it_would_overwrite() {
    let repo = fast_forwardable("ffdirty");
    let before = git(&repo, &["rev-parse", "refs/heads/main"]);
    write(&repo, "a", "local work\n");

    let out = run(&repo, &["merge", "feat"]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    assert_eq!(
        stderr(&out),
        "error: Your local changes to the following files would be overwritten by merge:\n\
         \ta\n\
         Please commit your changes or stash them before you merge.\n\
         Aborting\n"
    );
    // `cmd_merge` announces the fast-forward before attempting the checkout.
    assert!(
        stdout(&out).starts_with("Updating "),
        "the fast-forward was not announced before it was attempted: {}",
        stdout(&out)
    );
    assert!(!stdout(&out).contains("Fast-forward"));
    assert_eq!(read(&repo, "a"), "local work\n", "the refusal still wrote");
    assert_eq!(
        git(&repo, &["rev-parse", "refs/heads/main"]),
        before,
        "the branch moved even though the checkout was refused"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `verify_absent()`: an untracked file where the merge wants to write is its
/// own refusal, with its own advice line.
#[test]
fn a_fast_forward_refuses_an_untracked_file_in_the_way() {
    let repo = fast_forwardable("ffuntracked");
    write(&repo, "n", "squatter\n");

    let out = run(&repo, &["merge", "feat"]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    assert_eq!(
        stderr(&out),
        "error: The following untracked working tree files would be overwritten by merge:\n\
         \tn\n\
         Please move or remove them before you merge.\n\
         Aborting\n"
    );
    assert_eq!(read(&repo, "n"), "squatter\n");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// A fast-forward never reaches merge-ort's index gate, so staged work outside
/// its footprint survives it — content, staged state and all.
#[test]
fn a_fast_forward_keeps_staged_work_outside_its_footprint() {
    let repo = fast_forwardable("ffstaged");
    write(&repo, "b", "staged\n");
    git(&repo, &["add", "b"]);

    let out = run(&repo, &["merge", "feat"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(stdout(&out).contains("Fast-forward"));
    assert_eq!(read(&repo, "a"), "v2\n", "the fast-forward did not land");
    assert_eq!(
        git(&repo, &["status", "--porcelain"]),
        "M  b\n",
        "the fast-forward dropped a staged change it never touched"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// A three-way merge does reach the index gate, and it is all-or-nothing: even
/// a staged change the merge never touches stops it, with the one-line wording
/// `repo_index_has_changes()` builds and the strategy-failure exit code.
#[test]
fn a_three_way_merge_refuses_any_staged_change() {
    let repo = diverged("3wstaged");
    write(&repo, "b", "staged\n");
    git(&repo, &["add", "b"]);

    let out = run(&repo, &["merge", "--no-edit", "feat"]);
    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));
    assert_eq!(
        stderr(&out),
        "error: Your local changes to the following files would be overwritten by merge:\n\
         \x20\x20b\n\
         Merge with strategy ort failed.\n"
    );
    assert_eq!(stdout(&out), "", "the refused merge reported progress");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// An *unstaged* change outside the footprint is not the index gate's business,
/// and `twoway_merge()` never looks at the path — so the merge commit happens
/// and the change is still there.
#[test]
fn a_three_way_merge_carries_unrelated_local_changes_through() {
    let repo = diverged("3wdirty");
    write(&repo, "b", "local work\n");

    let out = run(&repo, &["merge", "--no-edit", "feat"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert_eq!(read(&repo, "a"), "v2\n", "the merge did not land");
    assert_eq!(read(&repo, "b"), "local work\n", "the merge clobbered a local change");
    assert_eq!(
        git(&repo, &["rev-list", "--count", "--merges", "HEAD"]).trim(),
        "1",
        "no merge commit was written"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// A path the three-way merge *does* rewrite is refused by the checkout, which
/// adds the strategy-failure line the fast-forward has no reason to print.
#[test]
fn a_three_way_merge_refuses_the_paths_it_would_overwrite() {
    let repo = diverged("3woverlap");
    write(&repo, "a", "local work\n");

    let out = run(&repo, &["merge", "--no-edit", "feat"]);
    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));
    assert_eq!(
        stderr(&out),
        "error: Your local changes to the following files would be overwritten by merge:\n\
         \ta\n\
         Please commit your changes or stash them before you merge.\n\
         Aborting\n\
         Merge with strategy ort failed.\n"
    );
    assert_eq!(read(&repo, "a"), "local work\n");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `-s ours` keeps our tree verbatim, so nothing is checked out over a dirty
/// file — but `merge-ours` still demands an index that matches `HEAD`, and says
/// nothing of its own when it does not.
#[test]
fn strategy_ours_minds_the_index_and_not_the_worktree() {
    let repo = diverged("ours");
    write(&repo, "b", "local work\n");

    let out = run(&repo, &["merge", "--no-edit", "-s", "ours", "feat"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert_eq!(read(&repo, "b"), "local work\n");
    assert_eq!(read(&repo, "a"), "v1\n", "-s ours took their side of a path");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());

    // A fresh repo: once the merge above lands, `feat` is reachable and the
    // up-to-date path preempts every gate.
    let repo = diverged("ours_staged");
    write(&repo, "b", "staged\n");
    git(&repo, &["add", "b"]);
    let out = run(&repo, &["merge", "--no-edit", "-s", "ours", "feat"]);
    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));
    assert_eq!(stderr(&out), "Merge with strategy ours failed.\n");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// The octopus is gated the same way — `git-merge-octopus` opens with
/// `diff-index --cached HEAD` — but prints its refusal itself, on stdout, in its
/// own four-space shape.
#[test]
fn an_octopus_minds_the_index_and_not_the_worktree() {
    let repo = diverged("octopus");
    git(&repo, &["checkout", "-q", "-b", "other", "main"]);
    write(&repo, "d", "other\n");
    git(&repo, &["add", "d"]);
    git(&repo, &["commit", "-q", "-m", "other"]);
    git(&repo, &["checkout", "-q", "main"]);

    write(&repo, "b", "local work\n");
    let out = run(&repo, &["merge", "--no-edit", "feat", "other"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert_eq!(read(&repo, "b"), "local work\n");
    assert_eq!(read(&repo, "d"), "other\n", "the octopus did not land");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());

    // Both heads are reachable after that merge, which would preempt the gate.
    let repo = diverged("octopus_staged");
    git(&repo, &["checkout", "-q", "-b", "other", "main"]);
    write(&repo, "d", "other\n");
    git(&repo, &["add", "d"]);
    git(&repo, &["commit", "-q", "-m", "other"]);
    git(&repo, &["checkout", "-q", "main"]);

    write(&repo, "b", "staged\n");
    git(&repo, &["add", "b"]);
    let out = run(&repo, &["merge", "--no-edit", "feat", "other"]);
    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "Error: Your local changes to the following files would be overwritten by merge\n    b\n"
    );
    assert_eq!(stderr(&out), "Merge with strategy octopus failed.\n");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
