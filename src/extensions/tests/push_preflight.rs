//! `zpush` network-free pre-flight: when the local `origin/main` remote-tracking
//! ref is ahead of HEAD (a prior fetch showed the remote moved), the push is
//! refused before enqueue with `pull first` — no daemon, no network needed.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.email=t@example.com", "-c", "user.name=zvcs-test"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn zpush_refuses_when_remote_tracking_is_ahead() {
    let root = std::env::temp_dir().join(format!("zvcs-preflight-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();

    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["commit", "--allow-empty", "-q", "-m", "c0"]);
    let c0 = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    git(&repo, &["commit", "--allow-empty", "-q", "-m", "c1"]);
    let c1 = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();

    // Simulate "remote moved ahead of us": origin/main = c1, HEAD = c0.
    git(&repo, &["update-ref", "refs/remotes/origin/main", &c1]);
    git(&repo, &["reset", "--hard", "-q", &c0]);

    let out = Command::new(BIN)
        .args(["zpush"])
        .current_dir(&repo)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);

    assert!(!out.status.success(), "zpush should have refused; stderr:\n{err}");
    assert!(
        err.contains("pull first"),
        "expected a pull-first refusal; stderr:\n{err}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn zpush_refuses_via_live_lsrefs_when_remote_moved() {
    // The working clone is NOT fetched, so its remote-tracking ref is stale; only
    // a live ls-refs against the remote can see it moved. This proves the network
    // pre-flight (not the network-free fallback) refuses the push.
    let root = std::env::temp_dir().join(format!("zvcs-lsref-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();

    let bare = root.join("remote.git");
    git(&root, &["init", "-q", "--bare", bare.to_str().unwrap()]);

    // Clone A: create c0, push.
    git(&root, &["clone", "-q", bare.to_str().unwrap(), "work"]);
    let work = root.join("work");
    git(&work, &["checkout", "-q", "-B", "main"]);
    git(&work, &["commit", "--allow-empty", "-q", "-m", "c0"]);
    git(&work, &["push", "-q", "origin", "main"]);

    // Clone B: advance the remote to c1 (work never sees it).
    git(&root, &["clone", "-q", bare.to_str().unwrap(), "work2"]);
    let work2 = root.join("work2");
    git(&work2, &["checkout", "-q", "main"]);
    git(&work2, &["commit", "--allow-empty", "-q", "-m", "c1"]);
    git(&work2, &["push", "-q", "origin", "main"]);

    // zpush in `work` (HEAD=c0, unfetched) must refuse via live ls-refs.
    let out = Command::new(BIN)
        .args(["zpush"])
        .current_dir(&work)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "zpush should refuse; stderr:\n{err}");
    assert!(
        err.contains("pull first"),
        "expected live-ls-refs refusal; stderr:\n{err}"
    );

    let _ = std::fs::remove_dir_all(&root);
}
