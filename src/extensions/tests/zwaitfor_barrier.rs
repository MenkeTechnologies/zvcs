//! `git zwaitfor` — the cross-repo barrier on *state*.
//!
//! A barrier is a gate that scripts hang deployments on ("wait until the tree
//! is clean, then ship"), so its failure modes are the expensive kind: waiting
//! forever, or passing when it should not. Two of the three cases here are
//! about the second.
//!
//! **The condition reads the daemon's cached `repo_status`, and nothing else
//! writes that table.** With no daemon maintaining it the table is empty, and
//! `clean` / `synced` are `all()` over an empty set — vacuously true. So
//! `git zwaitfor clean` returns 0 *immediately* against a tree of dirty repos
//! whenever the daemon is not running. That is measured below, not asserted
//! from reading the code, and it is pinned as behaviour rather than quietly
//! accepted: a barrier that answers "condition met" when it has no information
//! is the one shape a gate must not have, and a script cannot tell that answer
//! from a real one. Changing it — failing closed, or distinguishing "no data" —
//! is a decision about the verb's contract, so the test records today's answer
//! and says what it costs.
//!
//! The two properties that *are* determinate without a daemon are worth as
//! much: a usage error is reported before the poll loop rather than after the
//! timeout, and the timeout is honoured.

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
fn a_tree_wide_condition_passes_vacuously_when_no_daemon_maintains_the_status_cache() {
    // Measured, and recorded as a hazard rather than as a virtue.
    //
    // `clean` is `all()` over `repo_status`, and only the daemon's status
    // sweeper writes that table — `zreindex` records the repositories but not
    // their state. So with no daemon, the set is empty, `all()` is true, and
    // the barrier passes instantly over a tree that is visibly dirty. The
    // fixture's only repo has uncommitted work; stock `status` reports it.
    let (work, home, repo) = fixture("vacuous");
    let dirty = both(&run(&repo, &home, &["status", "--porcelain"]));
    assert!(dirty.contains("a.txt"), "the fixture is not dirty:\n{dirty}");

    let start = Instant::now();
    let out = run(&work, &home, &["zwaitfor", "clean", "--timeout", "5"]);
    assert!(
        out.status.success() && start.elapsed().as_secs() < 3,
        "the vacuous pass documented here no longer happens — if `zwaitfor` learned to \
         distinguish an empty status cache from a clean tree, that is an improvement and \
         this test should be rewritten to assert the new contract"
    );

    let _ = std::fs::remove_dir_all(work.parent().unwrap());
}
