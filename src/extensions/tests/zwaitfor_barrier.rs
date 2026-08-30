//! `git zwaitfor` — the cross-repo barrier on *state*.
//!
//! A barrier is a gate that scripts hang deployments on ("wait until the tree is
//! clean, then ship"), so its failure modes are the expensive kind: waiting
//! forever, or passing when it should not.
//!
//! It used to pass when it should not, and this file recorded that as a hazard.
//! The condition reads the daemon's cached `repo_status`, and `clean`/`synced`
//! were `all()` over those rows — `all()` over an empty set is true, so on a
//! machine where nothing maintains the cache the barrier returned 0 immediately
//! over a visibly dirty tree. A script could not tell that answer from a real
//! one.
//!
//! The condition now says what the man page always said: *every indexed repo*.
//! An indexed repository with no status row is one nothing has reported on, not
//! one that passes by absence, and an empty index is unobservable for the same
//! reason. Both wait, and the timeout is what tells the caller nothing is
//! reporting — an answer a script can act on.
//!
//! The two properties that were always determinate are kept: a usage error is
//! reported before the poll loop rather than after the timeout, and the timeout
//! is honoured.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Instant;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home.join("zvcs"))
        .env("ZVCS_SOCK", home.join("sock"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .output()
        .unwrap()
}

fn both(out: &Output) -> String {
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    s
}

/// One indexed repository, dirty on purpose.
fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-zwait-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let work = root.join("work");
    let home = root.join("home");
    std::fs::create_dir_all(home.join("zvcs")).unwrap();
    let repo = work.join("r1");
    std::fs::create_dir_all(&repo).unwrap();
    assert!(run(&repo, &home, &["init", "-q", "-b", "main", "."]).status.success());
    std::fs::write(repo.join("a.txt"), b"one\n").unwrap();
    assert!(run(&repo, &home, &["add", "a.txt"]).status.success());
    assert!(run(&repo, &home, &["commit", "-q", "-m", "first"]).status.success());
    let idx = both(&run(&work, &home, &["zreindex", "--sync", work.to_str().unwrap()]));
    assert!(idx.contains("indexed 1"), "{idx}");
    // Uncommitted work, so `clean` is genuinely false about the tree.
    std::fs::write(repo.join("a.txt"), b"dirty\n").unwrap();
    (work, home, repo)
}

#[test]
fn a_usage_error_is_reported_before_the_poll_loop_not_after_the_timeout() {
    // `<repo> <sha>` needs the sha. Validating inside the loop instead would
    // make a typo look like a condition that never comes true: the caller waits
    // out the whole timeout and then gets a usage message.
    let (work, home, _repo) = fixture("usage");
    let start = Instant::now();
    let out = run(&work, &home, &["zwaitfor", "r1", "--timeout", "30"]);
    let elapsed = start.elapsed();
    assert!(!out.status.success(), "a missing sha was accepted");
    assert!(
        elapsed.as_secs() < 5,
        "the usage error waited for the timeout ({elapsed:?}) — it is being raised inside the poll loop"
    );
    assert!(both(&out).contains("usage"), "no usage message:\n{}", both(&out));

    let _ = std::fs::remove_dir_all(work.parent().unwrap());
}

#[test]
fn an_unmet_condition_times_out_at_one_and_says_so() {
    let (work, home, _repo) = fixture("timeout");
    let start = Instant::now();
    let out = run(&work, &home, &["zwaitfor", "r1", "0000000000", "--timeout", "1"]);
    let elapsed = start.elapsed();

    assert_eq!(out.status.code(), Some(1), "a timeout must exit 1, not {:?}", out.status.code());
    assert!(both(&out).contains("timed out"), "no timeout message:\n{}", both(&out));
    // Honoured, and not rounded up to the 60s default.
    assert!(elapsed.as_secs() < 15, "--timeout was not honoured ({elapsed:?})");

    let _ = std::fs::remove_dir_all(work.parent().unwrap());
}

#[test]
fn a_tree_nothing_has_reported_on_is_not_treated_as_clean() {
    // The fixture's repository has uncommitted work and no status row, which is
    // the state of any tree on a machine where the daemon is not maintaining the
    // cache. The barrier must wait rather than answer from an empty table.
    let (work, home, repo) = fixture("unreported");
    let dirty = both(&run(&repo, &home, &["status", "--porcelain"]));
    assert!(dirty.contains("a.txt"), "the fixture is not dirty:\n{dirty}");

    let out = run(&work, &home, &["zwaitfor", "clean", "--timeout", "3"]);
    assert!(
        !out.status.success(),
        "the barrier passed over a tree nothing has reported on:\n{}",
        both(&out)
    );

    let _ = std::fs::remove_dir_all(work.parent().unwrap());
}

#[test]
fn the_condition_holds_only_once_every_indexed_repo_has_reported() {
    // Two repositories, both clean on disk. Reporting one of them is not the
    // tree: a barrier that answered from the reported subset would pass while
    // the other repository's state is still unknown.
    let root = std::env::temp_dir().join(format!("zvcs-zwait-partial-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let work = root.join("work");
    let home = root.join("home");
    std::fs::create_dir_all(home.join("zvcs")).unwrap();
    for name in ["r1", "r2"] {
        let r = work.join(name);
        std::fs::create_dir_all(&r).unwrap();
        assert!(run(&r, &home, &["init", "-q", "-b", "main", "."]).status.success());
        std::fs::write(r.join("a.txt"), b"one\n").unwrap();
        assert!(run(&r, &home, &["add", "a.txt"]).status.success());
        assert!(run(&r, &home, &["commit", "-q", "-m", "first"]).status.success());
    }
    let idx = both(&run(&work, &home, &["zreindex", "--sync", work.to_str().unwrap()]));
    assert!(idx.contains("indexed 2"), "{idx}");

    // One reported: not enough.
    assert!(run(&work.join("r1"), &home, &["zstatus"]).status.success());
    let partial = run(&work, &home, &["zwaitfor", "clean", "--timeout", "3"]);
    assert!(
        !partial.status.success(),
        "the barrier passed with only one of two repositories reported:\n{}",
        both(&partial)
    );

    // Both reported and both clean: met, and without burning the timeout.
    assert!(run(&work.join("r2"), &home, &["zstatus"]).status.success());
    let start = Instant::now();
    let full = run(&work, &home, &["zwaitfor", "clean", "--timeout", "10"]);
    assert!(full.status.success(), "a fully reported clean tree must pass:\n{}", both(&full));
    assert!(start.elapsed().as_secs() < 5, "a met condition must return at once, not at the timeout");

    // And a repository that is reported dirty holds the barrier closed.
    std::fs::write(work.join("r2/a.txt"), b"dirty\n").unwrap();
    assert!(run(&work.join("r2"), &home, &["zstatus"]).status.success());
    let dirty = run(&work, &home, &["zwaitfor", "clean", "--timeout", "3"]);
    assert!(!dirty.status.success(), "a reported-dirty repository must hold the barrier:\n{}", both(&dirty));

    let _ = std::fs::remove_dir_all(&root);
}
