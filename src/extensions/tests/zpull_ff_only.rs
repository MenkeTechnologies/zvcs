//! `git zpull` — fetch and fast-forward the whole indexed tree, in parallel.
//!
//! The verb runs `reconcile_repo` — the same ff-only reconcile `zsync` applies
//! to submodules — across every selected repository at once. What makes it
//! worth a test is not the fast-forward, which is ordinary, but the two states
//! it must *decline* to touch:
//!
//!  * a **dirty** repository, where a fast-forward would check out files over
//!    uncommitted work;
//!  * a **diverged** one, where the only ways forward are a merge or a force,
//!    and this verb is allowed to do neither.
//!
//! Both are asserted on repository state — HEAD, and the bytes in the file —
//! because both print a line either way, and a line is not evidence that
//! nothing happened. The whole fixture is local: a bare peer and clones of it,
//! so nothing here touches a network.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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

fn ok(out: &Output, what: &str) -> String {
    assert!(
        out.status.success(),
        "{what} failed ({}): {}{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn both(out: &Output) -> String {
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    s
}

fn subject(repo: &Path, home: &Path) -> String {
    ok(&run(repo, home, &["log", "-1", "--format=%s"]), "log").trim().to_string()
}

/// A bare peer with two commits, and clones stopped at the first one.
///
/// Returns `(work, home)`. `work/clean`, `work/dirty` and `work/diverged` are
/// each one commit behind the peer.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-zpull-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    std::fs::create_dir_all(home.join("zvcs")).unwrap();
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();

    ok(&run(&root, &home, &["init", "-q", "-b", "main", "--bare", "peer.git"]), "init peer");
    ok(&run(&root, &home, &["clone", "-q", "peer.git", "seed"]), "clone seed");
    let seed = root.join("seed");
    std::fs::write(seed.join("a.txt"), b"one\n").unwrap();
    ok(&run(&seed, &home, &["add", "a.txt"]), "add");
    ok(&run(&seed, &home, &["commit", "-qm", "first"]), "commit");
    ok(&run(&seed, &home, &["push", "-q", "origin", "main"]), "push");

    for name in ["clean", "dirty", "diverged"] {
        ok(&run(&work, &home, &["clone", "-q", "../peer.git", name]), "clone");
    }

    // Upstream moves on, so every clone is exactly one commit behind.
    std::fs::write(seed.join("a.txt"), b"two\n").unwrap();
    ok(&run(&seed, &home, &["commit", "-qam", "upstream"]), "upstream commit");
    ok(&run(&seed, &home, &["push", "-q", "origin", "main"]), "push upstream");

    (work, home)
}

#[test]
fn a_clean_repo_behind_its_upstream_is_fast_forwarded() {
    let (work, home) = fixture("ff");
    let clean = work.join("clean");
    assert_eq!(subject(&clean, &home), "first");

    ok(&run(&work, &home, &["zreindex", "--sync", work.to_str().unwrap()]), "reindex");
    let out = both(&run(&work, &home, &["zpull"]));
    assert!(out.contains("synced"), "no fast-forward reported:\n{out}");

    assert_eq!(subject(&clean, &home), "upstream", "the clone was not fast-forwarded");
    assert_eq!(
        std::fs::read_to_string(clean.join("a.txt")).unwrap(),
        "two\n",
        "the worktree was not updated with the ref"
    );

    let _ = std::fs::remove_dir_all(work.parent().unwrap());
}

#[test]
fn a_dirty_repo_is_skipped_with_its_uncommitted_work_intact() {
    // A fast-forward here would check out `two` over the local edit. The verb
    // must decline, and "decline" has to mean the bytes are still there.
    let (work, home) = fixture("dirty");
    let dirty = work.join("dirty");
    std::fs::write(dirty.join("a.txt"), b"local edit\n").unwrap();

    ok(&run(&work, &home, &["zreindex", "--sync", work.to_str().unwrap()]), "reindex");
    let out = both(&run(&work, &home, &["zpull"]));
    assert!(out.contains("dirty"), "the skip was not reported:\n{out}");

    assert_eq!(subject(&dirty, &home), "first", "a dirty repo was fast-forwarded");
    assert_eq!(
        std::fs::read_to_string(dirty.join("a.txt")).unwrap(),
        "local edit\n",
        "uncommitted work was overwritten by the pull"
    );

    let _ = std::fs::remove_dir_all(work.parent().unwrap());
}

#[test]
fn a_diverged_repo_is_never_forced() {
    // One local commit and one upstream commit: not a fast-forward. The only
    // ways on are a merge or a reset, and this verb may do neither.
    let (work, home) = fixture("diverged");
    let diverged = work.join("diverged");
    std::fs::write(diverged.join("b.txt"), b"local\n").unwrap();
    ok(&run(&diverged, &home, &["add", "b.txt"]), "add");
    ok(&run(&diverged, &home, &["commit", "-qm", "local only"]), "local commit");
    let before = subject(&diverged, &home);
    assert_eq!(before, "local only");

    ok(&run(&work, &home, &["zreindex", "--sync", work.to_str().unwrap()]), "reindex");
    let _ = run(&work, &home, &["zpull"]);

    assert_eq!(subject(&diverged, &home), before, "a diverged repo was moved");
    assert!(diverged.join("b.txt").exists(), "the local-only commit's file is gone");

    let _ = std::fs::remove_dir_all(work.parent().unwrap());
}

#[test]
fn a_second_pull_reports_up_to_date_and_changes_nothing() {
    let (work, home) = fixture("idempotent");
    ok(&run(&work, &home, &["zreindex", "--sync", work.to_str().unwrap()]), "reindex");
    ok(&run(&work, &home, &["zpull"]), "first pull");
    let after_first = subject(&work.join("clean"), &home);

    let out = both(&run(&work, &home, &["zpull"]));
    assert!(out.contains("up to date"), "a no-op pull did not say so:\n{out}");
    assert_eq!(subject(&work.join("clean"), &home), after_first, "a no-op pull moved HEAD");

    let _ = std::fs::remove_dir_all(work.parent().unwrap());
}
