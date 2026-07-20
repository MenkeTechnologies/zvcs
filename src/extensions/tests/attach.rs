//! Integration tests for detached-HEAD healing (`zvcs::superset::ensure_attached`).
//!
//! This is the primitive that ends the stash → `checkout -B main` → stash-pop
//! dance: `git submodule update` leaves a detached HEAD, and the daemon must
//! re-attach it to the mainline branch *without* touching the worktree — so it
//! is safe even when the worktree is dirty. The two tests below prove exactly
//! that: a clean detached HEAD is re-attached to `main` at the same commit, and
//! a dirty detached HEAD is re-attached with its uncommitted change preserved.

use std::path::{Path, PathBuf};
use std::process::Command;

use zvcs::superset::{ensure_attached, Attached};

/// Run `git <args>` in `dir` with a deterministic identity; panic on failure.
fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    let out = Command::new("git")
        .args(["-c", "user.email=test@example.com", "-c", "user.name=zvcs-test"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("failed to run git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

/// A fresh repo on branch `main` with a single root commit, returned canonicalized.
fn init_repo(tag: &str) -> PathBuf {
    let tmp = std::env::temp_dir().join(format!("zvcs-attach-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("mkdir tmp");
    let tmp = tmp.canonicalize().expect("canonicalize tmp");
    git(&tmp, &["init", "-q", "-b", "main"]);
    git(&tmp, &["commit", "--allow-empty", "-q", "-m", "root"]);
    tmp
}

fn head_sha(dir: &Path) -> String {
    String::from_utf8(git(dir, &["rev-parse", "HEAD"]).stdout)
        .unwrap()
        .trim()
        .to_string()
}

#[test]
fn reattaches_clean_detached_head_to_main() {
    let tmp = init_repo("clean");
    let before = head_sha(&tmp);

    // Detach HEAD at the current commit (what `git submodule update` produces).
    git(&tmp, &["checkout", "-q", "--detach"]);
    let repo = gix::discover(&tmp).expect("discover");
    assert!(
        repo.head_name().unwrap().is_none(),
        "precondition: HEAD must be detached before attach"
    );

    let outcome = ensure_attached(&repo).expect("ensure_attached");
    assert!(
        matches!(outcome, Attached::Attached { ref mainline } if mainline == "main"),
        "expected re-attachment to main"
    );

    // Re-open so we read the freshly written refs from disk.
    let repo = gix::discover(&tmp).expect("re-discover");
    let name = repo.head_name().unwrap().expect("HEAD must be symbolic now");
    assert_eq!(name.shorten().to_string(), "main", "HEAD must be on main");
    assert_eq!(head_sha(&tmp), before, "the checked-out commit must not move");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn reattaches_dirty_detached_head_without_clobbering() {
    let tmp = init_repo("dirty");
    let before = head_sha(&tmp);

    git(&tmp, &["checkout", "-q", "--detach"]);
    // Dirty the worktree AFTER detaching — this is the case the manual flow needs
    // a stash for. ensure_attached must preserve it untouched.
    std::fs::write(tmp.join("wip.txt"), b"in-flight work\n").expect("write wip");

    let repo = gix::discover(&tmp).expect("discover");
    let outcome = ensure_attached(&repo).expect("ensure_attached");
    assert!(
        matches!(outcome, Attached::Attached { .. }),
        "dirty detached HEAD must still attach"
    );

    // HEAD is on main at the same commit...
    let repo = gix::discover(&tmp).expect("re-discover");
    assert_eq!(
        repo.head_name().unwrap().unwrap().shorten().to_string(),
        "main"
    );
    assert_eq!(head_sha(&tmp), before, "commit must not move");
    // ...and the uncommitted change is still there, unstaged (no clobber).
    let status = String::from_utf8(git(&tmp, &["status", "--porcelain"]).stdout).unwrap();
    assert!(
        status.contains("wip.txt"),
        "the dirty file must be preserved as an uncommitted change; status was:\n{status}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn refuses_to_attach_when_branch_is_ahead() {
    // main is ahead of a detached HEAD → attaching would need a worktree move (or
    // move main backward), so ensure_attached refuses rather than clobber.
    let tmp = init_repo("ahead");
    let c0 = head_sha(&tmp);
    git(&tmp, &["commit", "--allow-empty", "-q", "-m", "c1"]); // main -> c1
    git(&tmp, &["checkout", "-q", "--detach", &c0]); // HEAD detached at c0, main=c1

    let repo = gix::discover(&tmp).expect("discover");
    let outcome = ensure_attached(&repo).expect("ensure_attached");
    assert!(matches!(outcome, Attached::Refused(_)), "must refuse when main is ahead of HEAD");

    // HEAD must remain detached (nothing was attached/clobbered).
    let repo = gix::discover(&tmp).expect("re-discover");
    assert!(repo.head_name().unwrap().is_none(), "HEAD must stay detached after refusal");

    let _ = std::fs::remove_dir_all(&tmp);
}
