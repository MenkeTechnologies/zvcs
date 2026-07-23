//! Reconcile must not clobber a mainline BRANCH that has unpushed commits, even
//! when HEAD is detached at an ancestor of the remote tip. The fast-forward guard
//! checks HEAD; without a separate guard on `refs/heads/<mainline>` a force-move
//! (PreviousValue::Any) would orphan the branch's commits — the very commits
//! `ensure_attached` refuses to touch. This is the `git submodule update`
//! detached-HEAD-with-local-work state the bots hit constantly.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    let out = Command::new("git")
        .args(["-c", "user.email=t@e.x", "-c", "user.name=t", "-c", "protocol.file.allow=always", "-c", "advice.detachedHead=false"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    out
}

fn rev(dir: &Path, r: &str) -> String {
    String::from_utf8(git(dir, &["rev-parse", r]).stdout).unwrap().trim().to_string()
}

#[test]
fn reconcile_does_not_clobber_diverged_local_branch() {
    let root = std::env::temp_dir().join(format!("zvcs-branchguard-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");

    let bare = root.join("remote.git");
    git(&root, &["init", "-q", "--bare", bare.to_str().unwrap()]);
    git(&root, &["clone", "-q", bare.to_str().unwrap(), "work"]);
    let work = root.join("work");
    git(&work, &["checkout", "-q", "-B", "main"]);
    git(&work, &["commit", "--allow-empty", "-q", "-m", "c0"]);
    git(&work, &["push", "-q", "origin", "main"]);
    let c0 = rev(&work, "HEAD");

    // Local main advances to c1 (NOT pushed).
    git(&work, &["commit", "--allow-empty", "-q", "-m", "c1-local-unpushed"]);
    let c1 = rev(&work, "HEAD");

    // origin/main advances to c2 (diverged from c1) via another clone.
    git(&root, &["clone", "-q", bare.to_str().unwrap(), "other"]);
    let other = root.join("other");
    git(&other, &["checkout", "-q", "main"]);
    git(&other, &["commit", "--allow-empty", "-q", "-m", "c2-remote"]);
    git(&other, &["push", "-q", "origin", "main"]);

    // Detach HEAD at c0 (an ancestor of c2) — main branch stays at c1.
    git(&work, &["checkout", "--detach", "-q", &c0]);
    assert_eq!(rev(&work, "refs/heads/main"), c1, "precondition: main at c1");

    // zup fetches origin (origin/main -> c2) and reconciles. The HEAD ff-check
    // passes (c0 ancestor of c2), but main (c1) is diverged from c2 and must NOT
    // be moved.
    let out = Command::new(BIN).args(["zup"]).current_dir(&work).env("ZVCS_HOME", &home).output().unwrap();
    let report = String::from_utf8_lossy(&out.stdout);
    assert!(report.contains("skipped"), "reconcile should skip the diverged branch:\n{report}");

    // The unpushed local commit c1 must still be on main — not orphaned by a
    // force-move to c2.
    assert_eq!(rev(&work, "refs/heads/main"), c1, "main was clobbered — unpushed commit c1 lost!");

    let _ = std::fs::remove_dir_all(&root);
}
