//! `git mv` for the two sources it cannot simply `lstat()` and rename: a
//! destination that *is* the source on a case-insensitive filesystem, and a
//! source that lives only in the index because it is outside the sparse-checkout
//! cone.
//!
//! Both are ordinary `cmd_mv()` paths rather than edge cases —
//! `git mv README.md readme.md` is how a file gets renamed to lower case on
//! macOS, and a sparse checkout is the only reason a tracked file is absent from
//! the worktree without being deleted.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .output()
        .expect("run binary")
}

fn ok(dir: &Path, args: &[&str]) -> Output {
    let out = run(dir, args);
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    out
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn scratch(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-mvcs-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("mkdir fixture");
    root
}

/// A case-only rename is a rename whose destination `lstat()`s as the source
/// itself wherever the filesystem folds case, so the destination-exists check
/// stands down for it (builtin/mv.c:421-423).
#[test]
fn a_case_only_rename_is_not_its_own_obstacle() {
    let dir = scratch("case");
    ok(&dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("README.md"), "hi\n").expect("write");
    ok(&dir, &["add", "README.md"]);
    ok(&dir, &["commit", "-qm", "x"]);
    // The check only stands down when git believes the filesystem folds case, and
    // `git init` records what it found; say so explicitly so the test is about
    // `mv` rather than about the filesystem it happens to run on.
    ok(&dir, &["config", "core.ignorecase", "true"]);

    let out = ok(&dir, &["mv", "README.md", "readme.md"]);
    assert!(stderr_of(&out).is_empty(), "{}", stderr_of(&out));
    assert_eq!(stdout_of(&ok(&dir, &["status", "--short"])), "R  README.md -> readme.md\n");

    // A destination that is a *different* file is still refused.
    std::fs::write(dir.join("other.md"), "o\n").expect("write");
    ok(&dir, &["add", "other.md"]);
    let clash = run(&dir, &["mv", "readme.md", "other.md"]);
    assert_eq!(clash.status.code(), Some(128));
    assert_eq!(
        stderr_of(&clash),
        "fatal: destination exists, source=readme.md, destination=other.md\n"
    );
}

/// A repository in cone mode with `inside/` checked out and `outside/` not, so
/// `outside/drop.txt` is tracked, absent from the worktree, and carries
/// `skip-worktree`.
fn sparse_fixture(tag: &str) -> PathBuf {
    let dir = scratch(tag);
    ok(&dir, &["init", "-q", "-b", "main"]);
    for sub in ["inside", "outside"] {
        std::fs::create_dir_all(dir.join(sub)).expect("mkdir");
    }
    std::fs::write(dir.join("inside").join("keep.txt"), "kept\n").expect("write");
    std::fs::write(dir.join("outside").join("drop.txt"), "dropped\n").expect("write");
    ok(&dir, &["add", "-A"]);
    ok(&dir, &["commit", "-qm", "base"]);
    ok(&dir, &["sparse-checkout", "set", "inside"]);
    assert!(!dir.join("outside").join("drop.txt").exists(), "the cone must have removed it");
    dir
}

#[test]
fn a_source_outside_the_cone_is_named_by_the_sparse_advice_not_called_a_bad_source() {
    let dir = sparse_fixture("advice");

    let out = run(&dir, &["mv", "outside/drop.txt", "inside/drop.txt"]);
    assert_eq!(out.status.code(), Some(1));
    let err = stderr_of(&out);
    assert!(
        err.starts_with("The following paths and/or pathspecs matched paths that exist\n"),
        "{err}"
    );
    assert!(err.contains("outside/drop.txt"), "{err}");
    assert!(!err.contains("bad source"), "{err}");
    // Nothing moved.
    assert!(stdout_of(&ok(&dir, &["ls-files"])).contains("outside/drop.txt"));
}

#[test]
fn sparse_moves_the_index_entry_and_checks_out_a_destination_back_inside_the_cone() {
    let dir = sparse_fixture("materialize");

    ok(&dir, &["mv", "--sparse", "outside/drop.txt", "inside/drop.txt"]);

    let entries = stdout_of(&ok(&dir, &["ls-files", "-t"]));
    assert!(entries.contains("H inside/drop.txt"), "no longer skip-worktree: {entries}");
    assert!(!entries.contains("outside/drop.txt"), "the old name is gone: {entries}");
    // Moved into the cone, so it is a file again.
    assert_eq!(
        std::fs::read_to_string(dir.join("inside").join("drop.txt")).expect("read"),
        "dropped\n"
    );
    assert_eq!(stdout_of(&ok(&dir, &["status", "--short"])), "R  outside/drop.txt -> inside/drop.txt\n");
}
