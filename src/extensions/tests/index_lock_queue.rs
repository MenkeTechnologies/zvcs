//! A foreign `.git/index.lock` must never surface gitoxide's immediate-fail
//! error: wait for it briefly, then queue the command as a job.
//!
//! The daemon's FIFO lane only serializes zvcs writers. Stock git, an IDE, or a
//! hook takes git's `O_EXCL` lockfile instead, which the lane cannot see, and the
//! ported index writer acquires it with `Fail::Immediately` — one attempt, no
//! wait. Before this rule, an overlap with any of those wrote
//! `Could not acquire lock for index file … after 1 attempt(s)` and lost the
//! command. Now: a short wait covers the millisecond case, and a lock that
//! outlasts the wait sends the command to the queue.
//!
//! Every test points `ZVCS_HOME` at its own temp dir, so no test touches the
//! developer's real ledger, socket, or daemon state. With no daemon reachable in
//! that home, `queue_verb` runs the job inline (still marked `ZVCS_QUEUED`), so
//! the assertions below hold identically on a dev box and in headless CI — what
//! is pinned is the ROUTING (the queueing notice, the loop guard), not whichever
//! execution the job lands in.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Run the zvcs binary in `dir` with an isolated `ZVCS_HOME` plus any extra env.
///
/// The git config is pinned to `/dev/null` as well, not just the home. Autostart
/// spawns a daemon when the config asks for autonomy/hooks/status, and that config
/// is read from the DEVELOPER's `~/.gitconfig` — so on a box with `[zvcs] autohook`
/// set, every one of these processes started a daemon in its temp home that
/// nothing ever stopped (four orphans per suite run), and the tests silently
/// exercised the daemon path instead of the no-daemon path they document.
fn zvcs(dir: &Path, home: &Path, env: &[(&str, &str)], args: &[&str]) -> Output {
    let mut cmd = Command::new(BIN);
    cmd.args(args)
        .current_dir(dir)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null");
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().expect("run zvcs git")
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A fresh repo with one unstaged file, plus its private `ZVCS_HOME`.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-idxlock-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    let home = root.join("home");
    std::fs::create_dir_all(&repo).expect("mkdir repo");
    std::fs::create_dir_all(&home).expect("mkdir home");

    assert!(zvcs(&repo, &home, &[], &["init", "-q", "-b", "main"]).status.success(), "init");
    std::fs::write(repo.join("a.txt"), b"hello\n").expect("write file");
    (repo, home)
}

/// Take the foreign lock the way stock git does — an exclusive create.
fn take_index_lock(repo: &Path) -> PathBuf {
    let lock = repo.join(".git").join("index.lock");
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock)
        .expect("take index.lock");
    lock
}

#[test]
fn waits_out_a_foreign_lock_that_clears() {
    let (repo, home) = fixture("wait");
    let lock = take_index_lock(&repo);

    // Release it while the add is inside its wait budget.
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(250));
        std::fs::remove_file(&lock).expect("release index.lock");
    });

    let out = zvcs(&repo, &home, &[("ZVCS_INDEX_LOCK_WAIT_MS", "5000")], &["add", "a.txt"]);
    releaser.join().expect("releaser");

    assert!(out.status.success(), "add should wait, not fail: {}", stderr_of(&out));
    assert!(
        !stderr_of(&out).contains("Could not acquire lock"),
        "gitoxide's immediate-fail error must not reach the user: {}",
        stderr_of(&out)
    );

    let staged = zvcs(&repo, &home, &[], &["status", "--short"]);
    assert!(
        String::from_utf8_lossy(&staged.stdout).contains("A  a.txt"),
        "the file is staged once the lock clears"
    );
}

#[test]
fn queues_when_the_lock_outlasts_the_wait() {
    let (repo, home) = fixture("queue");
    let _lock = take_index_lock(&repo); // never released

    let out = zvcs(&repo, &home, &[("ZVCS_INDEX_LOCK_WAIT_MS", "50")], &["add", "a.txt"]);

    assert!(
        stderr_of(&out).contains("index is locked by another writer — queueing"),
        "a stuck lock routes to the queue: {}",
        stderr_of(&out)
    );
}

#[test]
fn a_queued_rerun_never_requeues_itself() {
    let (repo, home) = fixture("loopguard");
    let _lock = take_index_lock(&repo); // never released

    // ZVCS_QUEUED marks the job's own re-run. It must fail outright rather than
    // spawn another job — otherwise a permanently stuck lock queues forever.
    let out = zvcs(
        &repo,
        &home,
        &[("ZVCS_INDEX_LOCK_WAIT_MS", "50"), ("ZVCS_QUEUED", "1")],
        &["add", "a.txt"],
    );

    assert!(!out.status.success(), "the re-run reports the failure");
    assert!(
        !stderr_of(&out).contains("queueing"),
        "the loop guard holds — no second job: {}",
        stderr_of(&out)
    );
}

#[test]
fn zero_wait_disables_the_budget() {
    let (repo, home) = fixture("nowait");
    let _lock = take_index_lock(&repo);

    let start = std::time::Instant::now();
    let out = zvcs(&repo, &home, &[("ZVCS_INDEX_LOCK_WAIT_MS", "0")], &["add", "a.txt"]);
    let elapsed = start.elapsed();

    assert!(
        stderr_of(&out).contains("queueing"),
        "still routes to the queue: {}",
        stderr_of(&out)
    );
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "a 0ms budget must not sleep through the default wait (took {elapsed:?})"
    );
}
