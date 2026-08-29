//! `git zpushall` — push every indexed repository that is ahead of its upstream.
//!
//! One property carries this verb, and it is the one a fleet-wide push had
//! better get right: **it never forces.** `zpushall` runs a plain `push`, so a
//! repository whose branch has diverged is rejected by the remote and reported
//! as a failure — the remote ref must be exactly where it was. A version that
//! reached for `--force` to make the summary line read better would discard
//! somebody else's commits across every repo in the tree at once.
//!
//! So the diverged case asserts the **peer's** ref, not the local one: the only
//! evidence that a push did not happen is that the thing being pushed to did
//! not move.
//!
//! The two supporting contracts are about being scriptable: a repository that
//! is not ahead is skipped rather than pushed, and a run with any failure exits
//! non-zero (verified as the process's own status — piping the verb into
//! `tail` reports the pipeline's exit, which is how this contract can look
//! satisfied when it is not).

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

/// The commit a bare repository's `main` points at.
fn peer_head(peer: &Path, home: &Path) -> String {
    ok(&run(peer, home, &["rev-parse", "main"]), "peer rev-parse").trim().to_string()
}

/// A bare peer plus exactly the clones a case asks for.
///
/// `wanted` is any of `ahead`, `diverged`, `level`. One clone per case rather
/// than all three in every fixture: running *any* command through this binary
/// indexes the repository it ran in, so a clone that merely exists in the
/// fixture is also selected by `zpushall`. The first version of this file
/// shared one fixture, and the legitimately-ahead clone's push moved the peer —
/// which the diverged case then read as evidence of a force-push. Isolating the
/// clones is what makes each assertion about the repository it names.
fn fixture(tag: &str, wanted: &[&str]) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-zpushall-{tag}-{}", std::process::id()));
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

    // A diverged clone must be taken *before* upstream moves on, so its later
    // commit is neither an ancestor nor a descendant of the remote tip.
    if wanted.contains(&"diverged") {
        ok(&run(&work, &home, &["clone", "-q", "../peer.git", "diverged"]), "clone diverged");
    }
    std::fs::write(seed.join("a.txt"), b"two\n").unwrap();
    ok(&run(&seed, &home, &["commit", "-qam", "upstream"]), "upstream commit");
    ok(&run(&seed, &home, &["push", "-q", "origin", "main"]), "push upstream");

    // These clone *after* it, so one can be purely ahead and one exactly level.
    for name in ["ahead", "level"] {
        if wanted.contains(&name) {
            ok(&run(&work, &home, &["clone", "-q", "../peer.git", name]), "clone");
        }
    }

    if wanted.contains(&"ahead") {
        let ahead = work.join("ahead");
        std::fs::write(ahead.join("b.txt"), b"local\n").unwrap();
        ok(&run(&ahead, &home, &["add", "b.txt"]), "add");
        ok(&run(&ahead, &home, &["commit", "-qm", "ready to push"]), "commit ahead");
    }
    if wanted.contains(&"diverged") {
        let diverged = work.join("diverged");
        std::fs::write(diverged.join("c.txt"), b"local\n").unwrap();
        ok(&run(&diverged, &home, &["add", "c.txt"]), "add");
        ok(&run(&diverged, &home, &["commit", "-qm", "diverging"]), "commit diverged");
    }

    (work, home, root.join("peer.git"))
}

#[test]
fn a_repo_that_is_ahead_is_pushed_and_the_peer_moves() {
    let (work, home, peer) = fixture("ahead", &["ahead"]);
    ok(&run(&work, &home, &["zreindex", "--sync", work.to_str().unwrap()]), "reindex");
    let before = peer_head(&peer, &home);

    let out = both(&run(&work, &home, &["zpushall"]));
    assert!(out.contains("1 ok"), "the push was not counted:\n{out}");
    assert_ne!(peer_head(&peer, &home), before, "the peer did not receive the push");

    let _ = std::fs::remove_dir_all(work.parent().unwrap());
}

#[test]
fn a_diverged_repo_is_rejected_and_the_remote_is_left_where_it_was() {
    // The property: no `--force`, ever. The evidence is the peer's ref.
    let (work, home, peer) = fixture("diverged", &["diverged"]);
    let diverged = work.join("diverged");
    ok(&run(&work, &home, &["zreindex", "--sync", work.to_str().unwrap()]), "reindex");
    let before = peer_head(&peer, &home);
    let local = ok(&run(&diverged, &home, &["rev-parse", "HEAD"]), "rev-parse").trim().to_string();

    let out = run(&work, &home, &["zpushall"]);
    let text = both(&out);
    assert!(text.contains("failed"), "the rejection was not reported:\n{text}");
    assert_eq!(
        peer_head(&peer, &home),
        before,
        "a diverged repository was force-pushed — the remote tip moved"
    );
    // Stated the other way too, so the case still means something if a future
    // fixture ever carries more than one repository: the local commit must not
    // have reached the peer by any route.
    let on_peer = run(&peer, &home, &["cat-file", "-e", &local]);
    assert!(!on_peer.status.success(), "the diverged commit reached the remote");
    // And the local commit is still there: nothing was rewritten to make the
    // push succeed either.
    assert_eq!(
        ok(&run(&diverged, &home, &["log", "-1", "--format=%s"]), "log").trim(),
        "diverging"
    );

    let _ = std::fs::remove_dir_all(work.parent().unwrap());
}

#[test]
fn a_failure_makes_the_run_exit_non_zero() {
    // Checked as the process's own status. Reading it after a pipe reports the
    // pipeline's exit instead, which makes this contract look satisfied when it
    // is not.
    let (work, home, _peer) = fixture("exit", &["diverged"]);
    ok(&run(&work, &home, &["zreindex", "--sync", work.to_str().unwrap()]), "reindex");

    let out = run(&work, &home, &["zpushall"]);
    assert_eq!(out.status.code(), Some(1), "a failed push exited {:?}", out.status.code());

    let _ = std::fs::remove_dir_all(work.parent().unwrap());
}

#[test]
fn a_repo_level_with_its_upstream_is_skipped_rather_than_pushed() {
    let (work, home, peer) = fixture("level", &["level"]);
    ok(&run(&work, &home, &["zreindex", "--sync", work.to_str().unwrap()]), "reindex");
    let before = peer_head(&peer, &home);

    let out = run(&work, &home, &["zpushall"]);
    let text = both(&out);
    assert!(text.contains("skipped"), "a level repo was not skipped:\n{text}");
    assert!(out.status.success(), "skipping counted as a failure:\n{text}");
    assert_eq!(peer_head(&peer, &home), before, "a level repo still pushed something");

    let _ = std::fs::remove_dir_all(work.parent().unwrap());
}
