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
