//! The fan-out mutations — `zfetch`, `zgc`, `zfsck`, `zprune` — run one git
//! operation across every selected repository in parallel through a shared
//! `fan_out`.
//!
//! What matters for a fleet mutation is not the happy path but what it does with
//! a repository that fails: the run must finish the others, count the failure,
//! exit non-zero, and leave the failure discoverable afterwards. A fleet verb
//! that reports success while one repo silently failed is the shape that makes
//! people stop trusting the whole tree.
//!
//! Also pinned: a repository with no remote at all is not a failure. Stock git
//! exits 0 for `fetch` there, so a mixed fleet of clones and local-only repos
//! must not turn every run red.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(home: &Path, cwd: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap()
}

fn git(home: &Path, cwd: &Path, args: &[&str]) {
    let out = run(home, cwd, args);
    assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
}

fn stdout(home: &Path, cwd: &Path, args: &[&str]) -> String {
    String::from_utf8_lossy(&run(home, cwd, args).stdout).trim().to_string()
}

/// stdout + stderr + exit code: the summary line lands on stderr, the per-repo
/// blocks on stdout, and the gate is the code.
fn fleet(home: &Path, cwd: &Path, args: &[&str]) -> (String, i32) {
    let out = run(home, cwd, args);
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    (s, out.status.code().unwrap_or(-1))
}

/// A bare origin with one commit, plus `n` clones of it, all indexed. Returns
/// the root and the ZVCS_HOME.
fn cloned_fleet(tag: &str, clones: &[&str]) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-fanout-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");

    let origin = root.join("origin.git");
    git(&home, &root, &["init", "-q", "--bare", "-b", "main", origin.to_str().unwrap()]);
    let seed = root.join("seed");
    git(&home, &root, &["clone", "-q", origin.to_str().unwrap(), seed.to_str().unwrap()]);
    git(&home, &seed, &["config", "user.email", "t@example"]);
    git(&home, &seed, &["config", "user.name", "T"]);
    std::fs::write(seed.join("f.txt"), b"1\n").unwrap();
    git(&home, &seed, &["add", "f.txt"]);
    git(&home, &seed, &["commit", "-q", "-m", "c0"]);
    git(&home, &seed, &["push", "-q", "origin", "main"]);

    for name in clones {
        git(&home, &root, &["clone", "-q", origin.to_str().unwrap(), root.join(name).to_str().unwrap()]);
    }
    (root, home)
}

/// Add a commit to origin so the clones fall behind.
fn advance_origin(home: &Path, root: &Path) -> String {
    let seed = root.join("seed");
    std::fs::write(seed.join("f.txt"), b"2\n").unwrap();
    git(home, &seed, &["commit", "-q", "-am", "c1"]);
    git(home, &seed, &["push", "-q", "origin", "main"]);
    stdout(home, &seed, &["rev-parse", "HEAD"])
}

#[test]
fn zfetch_advances_tracking_refs_without_touching_the_worktree() {
    let (root, home) = cloned_fleet("fetch", &["one", "two"]);
    let (one, two) = (root.join("one"), root.join("two"));
    let before_head = stdout(&home, &one, &["rev-parse", "HEAD"]);
    let pushed = advance_origin(&home, &root);
    run(&home, &root, &["zreindex", "--sync", root.to_str().unwrap()]);

    let (out, rc) = fleet(&home, &root, &["zfetch"]);
    assert_eq!(rc, 0, "a healthy fleet fetch must succeed:\n{out}");

    // Every clone's remote-tracking ref reaches the pushed commit...
    for repo in [&one, &two] {
        assert_eq!(stdout(&home, repo, &["rev-parse", "origin/main"]), pushed,
            "{} did not fetch", repo.display());
    }
    // ...while the local branch and worktree stay exactly where they were: fetch
    // is not pull, and a fleet verb that quietly moved HEAD would be a disaster.
    assert_eq!(stdout(&home, &one, &["rev-parse", "HEAD"]), before_head, "zfetch must not move HEAD");
    assert_eq!(std::fs::read_to_string(one.join("f.txt")).unwrap(), "1\n", "zfetch must not touch the worktree");

    // Output is grouped per repository, so a fleet run stays readable.
    assert!(out.contains(&format!("== {} ==", one.display())), "per-repo header missing:\n{out}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn one_broken_repo_fails_the_run_without_stopping_the_others() {
    let (root, home) = cloned_fleet("partial", &["one", "broken"]);
    let (one, broken) = (root.join("one"), root.join("broken"));
    // A remote that cannot be read: the fetch for this repo fails, every other
    // repo must still be fetched.
    git(&home, &broken, &["remote", "set-url", "origin", root.join("gone.git").to_str().unwrap()]);
    let pushed = advance_origin(&home, &root);
    run(&home, &root, &["zreindex", "--sync", root.to_str().unwrap()]);

    let (out, rc) = fleet(&home, &root, &["zfetch"]);
    assert_eq!(rc, 1, "a failed repo must fail the run:\n{out}");
    assert!(out.contains("1 failed"), "the summary must count the failure:\n{out}");
    assert_eq!(stdout(&home, &one, &["rev-parse", "origin/main"]), pushed,
        "a healthy repo must still be fetched after a sibling failed:\n{out}");

    // The failure outlives the command: `fan_out` records it, so `zjobs` can
    // still show which repo failed and at what.
    let jobs = stdout(&home, &root, &["zjobs"]);
    assert!(jobs.contains("zfetch") && jobs.contains("failed"),
        "the failure must be recorded for later inspection:\n{jobs}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_repo_with_no_remote_is_not_a_failure() {
    let (root, home) = cloned_fleet("noremote", &["one"]);
    // A local-only repo, the common case on a real tree: `git fetch` there is a
    // no-op that exits 0 in stock git, so it must not redden the fleet run.
    let solo = root.join("solo");
    std::fs::create_dir_all(&solo).unwrap();
    git(&home, &solo, &["init", "-q", "-b", "main"]);
    git(&home, &solo, &["config", "user.email", "t@example"]);
    git(&home, &solo, &["config", "user.name", "T"]);
    git(&home, &solo, &["commit", "-q", "--allow-empty", "-m", "solo"]);
    run(&home, &root, &["zreindex", "--sync", root.to_str().unwrap()]);

    let (out, rc) = fleet(&home, &root, &["zfetch"]);
    assert_eq!(rc, 0, "a remote-less repo must not fail the fleet:\n{out}");
    assert!(out.contains("0 failed"), "nothing should be counted as failed:\n{out}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn zprune_removes_unreachable_objects_across_the_fleet() {
    let (root, home) = cloned_fleet("prune", &["one"]);
    let one = root.join("one");
    git(&home, &one, &["config", "user.email", "t@example"]);
    git(&home, &one, &["config", "user.name", "T"]);

    // Orphan a commit: make it, reset off it, then drop the reflog that still
    // holds it. Without the expiry it is reachable and prune must not take it.
    std::fs::write(one.join("f.txt"), b"doomed\n").unwrap();
    git(&home, &one, &["commit", "-q", "-am", "doomed"]);
    let doomed = stdout(&home, &one, &["rev-parse", "HEAD"]);
    git(&home, &one, &["reset", "-q", "--hard", "HEAD~1"]);
    git(&home, &one, &["reflog", "expire", "--expire=now", "--all"]);
    assert_eq!(stdout(&home, &one, &["cat-file", "-t", &doomed]), "commit",
        "precondition: the orphan is still in the object store");

    run(&home, &root, &["zreindex", "--sync", root.to_str().unwrap()]);
    let (out, rc) = fleet(&home, &root, &["zprune", "one"]);
    assert_eq!(rc, 0, "prune must succeed:\n{out}");
    assert!(out.contains("1 ok"), "exactly the selected repo must be pruned:\n{out}");

    let after = run(&home, &one, &["cat-file", "-t", &doomed]);
    assert!(!after.status.success(), "the unreachable object survived zprune");

    let _ = std::fs::remove_dir_all(&root);
}
