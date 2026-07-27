//! A `git` CHILD of a `git` parent must never turn into a queued job.
//!
//! The repo lane is per-process, so a porcelain command that holds it and then
//! spawns a child `git` used to hand that child a `BUSY` lane. The child's
//! dispatch answered `BUSY` by submitting itself as a background job, printing
//! `zvcs: queued job #N` and exiting **0** — so the parent read success on work
//! that never ran. Two commands shipped broken this way: `commit -p` discarded
//! every selected hunk (the selector's `git apply` children were swallowed) and
//! `clone --recurse-submodules` returned 0 with empty submodules (the `submodule
//! update` child was swallowed).
//!
//! Neither is visible to a parity corpus: stock git and zvcs both exit 0. What
//! separates them is the EFFECT, so that is what this test asserts.
//!
//! One `#[test]` running three phases against one daemon, deliberately: the
//! phases share process-wide env (`ZVCS_SOCK`/`ZVCS_HOME`), which parallel test
//! threads would race on.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};
use std::thread;
use std::time::{Duration, Instant};

use zvcs::lock::RepoLock;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .expect("spawn git")
}

fn ok(dir: &Path, args: &[&str]) -> Output {
    let out = git(dir, args);
    assert!(out.status.success(), "git {args:?} failed: {}", text(&out));
    out
}

/// A child's combined output — the `queued job` notice goes to stderr, its real
/// output to stdout, and a swallowed child is recognized by either.
fn text(out: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

fn init_repo(root: &Path, name: &str) -> PathBuf {
    let r = root.join(name);
    std::fs::create_dir_all(&r).unwrap();
    ok(&r, &["init", "-q", "."]);
    r
}

#[test]
fn a_git_child_of_a_lane_holding_git_parent_does_real_work() {
    let root = std::env::temp_dir().join(format!("zvcs-clane-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let sock = root.join("s");
    // Isolate the singleton socket and ledger so this never touches (or waits on)
    // a real daemon the developer already has running.
    std::env::set_var("ZVCS_SOCK", &sock);
    std::env::set_var("ZVCS_HOME", root.join("home"));

    let mut daemon: Child = Command::new(BIN)
        .args(["zdaemon", "start", "--foreground"])
        .current_dir(&root)
        .spawn()
        .unwrap();
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(10) && !sock.exists() {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(sock.exists(), "daemon socket never appeared");

    let outcome = std::panic::catch_unwind(|| {
        phase_direct_child(&root);
        phase_recursive_clone(&root);
        phase_unrelated_holder(&root);
    });

    let _ = Command::new(BIN).args(["zdaemon", "stop"]).current_dir(&root).status();
    let _ = daemon.kill();
    let _ = daemon.wait();
    let _ = std::fs::remove_dir_all(&root);

    if let Err(e) = outcome {
        std::panic::resume_unwind(e);
    }
}

/// The mechanism, minimally: this process holds the lane and spawns a `git`
/// child on the same repo. Pre-fix the child answered `BUSY` by queueing itself.
///
/// Asserting on the INDEX, not the exit status, is the whole point — the broken
/// child also exited 0.
fn phase_direct_child(root: &Path) {
    let repo = init_repo(root, "direct");
    std::fs::write(repo.join("f"), "content\n").unwrap();
    let git_dir = repo.join(".git").canonicalize().unwrap();

    let lock = RepoLock::acquire(&git_dir);
    assert!(lock.is_held(), "the live daemon must grant the parent's lane");

    // The child cannot see this thread's reentrancy set; only the daemon can tell
    // it that the lane's holder is its own parent.
    let child = git(&repo, &["add", "f"]);
    let said = text(&child);
    drop(lock);

    assert!(
        !said.contains("queued job"),
        "the child queued itself instead of staging: {said}"
    );
    assert!(child.status.success(), "child `git add` failed: {said}");
    let staged = String::from_utf8_lossy(&ok(&repo, &["ls-files", "--cached"]).stdout).into_owned();
    assert!(
        staged.lines().any(|l| l == "f"),
        "child `git add` exited 0 but staged nothing (index: {staged:?})"
    );
}

/// Sighting 2, literally: `clone --recurse-submodules` holds the new repo's lane
/// across the `submodule update` child it re-execs. Pre-fix the child queued
/// itself, clone returned 0, and the submodule working tree stayed empty.
fn phase_recursive_clone(root: &Path) {
    let sub = init_repo(root, "sub");
    std::fs::write(sub.join("s"), "sub\n").unwrap();
    ok(&sub, &["add", "s"]);
    ok(&sub, &["commit", "-qm", "sub"]);

    let sup = init_repo(root, "sup");
    std::fs::write(sup.join("t"), "top\n").unwrap();
    ok(&sup, &["add", "t"]);
    ok(&sup, &["commit", "-qm", "top"]);
    // `protocol.file.allow` is git's file-transport gate for submodules.
    let add = git(
        &sup,
        &["-c", "protocol.file.allow=always", "submodule", "add", sub.to_str().unwrap(), "mod"],
    );
    assert!(add.status.success(), "submodule add failed: {}", text(&add));
    ok(&sup, &["add", "."]);
    ok(&sup, &["commit", "-qm", "addsub"]);

    let dst = root.join("clone");
    let out = git(
        root,
        &[
            "-c",
            "protocol.file.allow=always",
            "clone",
            "--recurse-submodules",
            "-q",
            sup.to_str().unwrap(),
            dst.to_str().unwrap(),
        ],
    );
    let said = text(&out);
    assert!(out.status.success(), "recursive clone failed: {said}");
    assert!(
        !said.contains("queued job"),
        "the `submodule update` child queued itself: {said}"
    );
    assert!(
        dst.join("mod/s").is_file(),
        "clone --recurse-submodules exited 0 with an EMPTY submodule ({said})"
    );
}

/// The other half of the guard. When the lane is held by a process that is NOT
/// an ancestor, a synchronous child still must not become a job — there is no
/// reentrancy to grant, so the only correct answer is to WAIT and then do the
/// work. Here the holder is a sibling (`commit` parked in a slow `pre-commit`
/// hook), and the child opts in with `ZVCS_NO_QUEUE` the way a caller across a
/// boundary this process cannot introspect would.
fn phase_unrelated_holder(root: &Path) {
    let repo = init_repo(root, "sibling");
    std::fs::write(repo.join("a"), "a\n").unwrap();
    ok(&repo, &["add", "a"]);
    ok(&repo, &["commit", "-qm", "one"]);

    let hooks = repo.join(".git/hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    std::fs::write(hooks.join("pre-commit"), "#!/bin/sh\nsleep 2\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(hooks.join("pre-commit"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }

    std::fs::write(repo.join("b"), "b\n").unwrap();
    ok(&repo, &["add", "b"]);
    // Sibling holder: `commit` takes the lane for the whole command, hook included.
    let mut holder = Command::new(BIN)
        .args(["commit", "-qm", "two"])
        .current_dir(&repo)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .spawn()
        .unwrap();
    thread::sleep(Duration::from_millis(600)); // let it reach the hook holding the lane

    std::fs::write(repo.join("c"), "c\n").unwrap();
    let child = Command::new(BIN)
        .args(["add", "c"])
        .current_dir(&repo)
        .env("ZVCS_NO_QUEUE", "1")
        .output()
        .expect("spawn git");
    let said = text(&child);
    let _ = holder.wait();

    assert!(
        !said.contains("queued job"),
        "ZVCS_NO_QUEUE child queued itself instead of waiting: {said}"
    );
    assert!(child.status.success(), "child `git add` failed: {said}");
    let staged = String::from_utf8_lossy(&ok(&repo, &["ls-files", "--cached"]).stdout).into_owned();
    assert!(
        staged.lines().any(|l| l == "c"),
        "child `git add` exited 0 but staged nothing (index: {staged:?})"
    );
}
