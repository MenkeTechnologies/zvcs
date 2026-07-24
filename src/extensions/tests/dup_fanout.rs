//! Dup fan-out — `git zsync` fast-forwards every LOCAL dup of a repo (another
//! checkout with the same `origin` URL) to the current checkout's HEAD, offline
//! and in parallel. A commit in one clone propagates to all its clones on the
//! machine with no network access.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(["-c", "user.email=t@e.x", "-c", "user.name=t"])
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?} failed in {}", dir.display());
}

fn head(dir: &Path) -> String {
    let out = Command::new("git").args(["rev-parse", "HEAD"]).current_dir(dir).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn zsync_fans_a_commit_out_to_all_local_dups() {
    let root = std::env::temp_dir().join(format!("zvcs-dup-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");

    // A source repo → bare origin → three clones a, b, c (all share the origin URL).
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    git(&src, &["init", "-q", "-b", "main"]);
    std::fs::write(src.join("x.txt"), "r1\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-q", "-m", "r1"]);
    git(&root, &["clone", "-q", "--bare", src.to_str().unwrap(), "origin.git"]);
    let origin = root.join("origin.git");
    for name in ["a", "b", "c"] {
        git(&root, &["clone", "-q", origin.to_str().unwrap(), name]);
    }
    let (a, b, c) = (root.join("a"), root.join("b"), root.join("c"));

    // Commit r2 in `a` ONLY — never pushed anywhere. Purely local.
    std::fs::write(a.join("x.txt"), "r2\n").unwrap();
    git(&a, &["add", "."]);
    git(&a, &["commit", "-q", "-m", "r2"]);
    let r2 = head(&a);
    assert_ne!(head(&b), r2);

    // Index the three dups so the fan-out can find them.
    let ok = Command::new(BIN)
        .args(["zreindex", "--sync", a.to_str().unwrap(), b.to_str().unwrap(), c.to_str().unwrap()])
        .env("ZVCS_HOME", &home)
        .status()
        .unwrap()
        .success();
    assert!(ok, "zreindex failed");

    // `git zsync` in `a` → fan the r2 commit out to b and c, offline.
    let out = Command::new(BIN).args(["zsync"]).current_dir(&a).env("ZVCS_HOME", &home).output().unwrap();
    assert!(out.status.success(), "zsync failed: {}", String::from_utf8_lossy(&out.stderr));

    assert_eq!(head(&b), r2, "dup b did not fast-forward to the source commit");
    assert_eq!(head(&c), r2, "dup c did not fast-forward to the source commit");
    assert_eq!(std::fs::read_to_string(b.join("x.txt")).unwrap(), "r2\n", "b worktree not updated");
    assert_eq!(std::fs::read_to_string(c.join("x.txt")).unwrap(), "r2\n", "c worktree not updated");

    let _ = std::fs::remove_dir_all(&root);
}
